//! The turn loop: what the engine does between the session's head and the
//! model's answer.
//!
//! The loop reads the session, asks the host's interceptors what the turn
//! does next, builds the request, commits the answer and repeats until the
//! turn ends. The host gets the last word: a message it was holding keeps the
//! turn alive, and it may stop the turn with a reason.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use norte_core::domain::commit::Commit;
use norte_core::domain::message::Message;
use norte_core::domain::node::Node;
use norte_core::domain::part::ToolResultPart;
use norte_core::domain::role::Role;
use norte_core::domain::session::{Phase, Session};
use norte_core::error::{Error, Result};
use norte_core::event::EngineEvent;
use norte_core::id::{NodeId, SessionId};
use norte_core::interceptor::{Interception, InterceptionContext};
use norte_core::model::TurnOptions;

use super::{ChannelSink, Consultation, Harness, TurnOutcome, session_turn_options, turn_metadata};

impl Harness {
    /// Walks the session forward until the turn ends.
    #[allow(clippy::too_many_lines)] // the loop and its exit conditions read as one.
    pub(super) async fn run(
        self,
        session_id: SessionId,
        input: Option<Message>,
        options: TurnOptions,
        sender: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<TurnOutcome> {
        let mut session = self.store.load(session_id).await?;
        let effective =
            options.over(&session_turn_options(&session).over(&self.default_turn_options()));
        if let Some(message) = input {
            // The host may add messages before the input, so they come first.
            // A parked session still owes a tool result, and the messages must
            // not come between the call and it.
            if session.phase == Phase::Idle {
                let history = self.store.history(session.head).await?;
                let consultation = self.consult(&session, &sender, Some(&history)).await?;
                session = consultation.session;
                if consultation.stop.is_some() {
                    return Ok(TurnOutcome {
                        session,
                        message: None,
                    });
                }
            }
            session = self.append_turn(&session, message, &sender).await?;
        }

        let mut last = None;
        let mut early_results: Vec<ToolResultPart> = Vec::new();
        let mut iterations = 0usize;
        loop {
            iterations += 1;
            if iterations > self.config.max_iterations {
                return Err(Error::InvalidInput(format!(
                    "agent exceeded {} iterations",
                    self.config.max_iterations
                )));
            }

            // A turn ends at the first assistant message that is not waiting on
            // tools; a parked session carries on as soon as the host has nothing
            // left to decide. The host gets the last word: a message it was
            // holding keeps the turn alive.
            let head_message = match session.head {
                Some(head) => Some(self.store.node(head).await?.message),
                None => None,
            };
            if let Some(message) = &head_message {
                if message.role == Role::Assistant && !self.continues_turn(&session).await? {
                    let Some(outcome) = self
                        .end_of_turn(&mut session, &sender, last.clone(), message)
                        .await?
                    else {
                        continue;
                    };
                    return Ok(outcome);
                }
            }

            match session.phase {
                Phase::AwaitingTool | Phase::AwaitingApproval => {
                    session = self
                        .run_tools(&session, &sender, &cancellation, &early_results)
                        .await?;
                    early_results.clear();
                    if cancellation.is_cancelled() {
                        return Ok(TurnOutcome {
                            session,
                            message: last,
                        });
                    }
                }
                Phase::Idle | Phase::Streaming => {
                    // The host may add messages before this request is built.
                    // The history is read once and the request reuses it.
                    let Some((next, head, history)) =
                        self.prepare_request(&session, &sender).await?
                    else {
                        return Ok(TurnOutcome {
                            session,
                            message: last,
                        });
                    };
                    session = next;
                    let (message, interrupted, pending) = self
                        .generate(&session, history, &effective, &sender, &cancellation)
                        .await?;
                    early_results = pending;
                    self.commit_answer(
                        &mut session,
                        &message,
                        head,
                        interrupted,
                        &effective,
                        &sender,
                    )
                    .await?;
                    last = Some(message);
                    if interrupted {
                        return Ok(TurnOutcome {
                            session,
                            message: last,
                        });
                    }
                    if session.phase == Phase::AwaitingTool && !self.config.auto_execute_tools {
                        return Ok(TurnOutcome {
                            session,
                            message: last,
                        });
                    }
                }
                // `Phase` is `#[non_exhaustive]`. A phase this build does not
                // know is one whose work it cannot do, so the loop stops here
                // rather than guessing what to run next.
                other => {
                    tracing::warn!(?other, "unknown session phase; ending the turn");
                    return Ok(TurnOutcome {
                        session,
                        message: last,
                    });
                }
            }
        }
    }

    /// Whether the loop keeps working from the current head.
    ///
    /// The turn ends at a final assistant message. It carries on while tools are
    /// pending, and a parked session carries on too once the host has nothing
    /// left to decide: a granted call runs, a denied one fails, and a call with
    /// no record at all asks again — which is how an imported or expired
    /// session finds its way forward (FR-APPR-5).
    async fn continues_turn(&self, session: &Session) -> Result<bool> {
        let Some(head) = session.head else {
            return Ok(false);
        };
        let message = self.store.node(head).await?.message;
        let mid_flight = matches!(session.phase, Phase::AwaitingTool | Phase::AwaitingApproval)
            && message.has_tool_calls();
        if !mid_flight {
            return Ok(false);
        }
        if session.phase == Phase::AwaitingTool {
            return Ok(true);
        }
        Ok(self.pending_approvals(session.id).await?.is_empty())
    }

    /// Commits the answer the model produced.
    ///
    /// The phase follows the answer: an interrupted one leaves the session
    /// idle, an answer with tool calls waits for them, and a final answer ends
    /// the turn.
    async fn commit_answer(
        &self,
        session: &mut Session,
        message: &Message,
        head: NodeId,
        interrupted: bool,
        options: &TurnOptions,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<()> {
        let node = Node::message(message.clone(), Some(head));
        let node_id = node.id;
        let phase = if interrupted {
            Phase::Idle
        } else if message.has_tool_calls() {
            Phase::AwaitingTool
        } else {
            Phase::Idle
        };
        *session = self
            .commit(
                Commit {
                    session_id: session.id,
                    expected_version: Some(session.version),
                    nodes: vec![node],
                    head: Some(node_id),
                    phase,
                    turn: session.turn,
                    grants: session.grants.clone(),
                    metadata: Some(turn_metadata(session, options)),
                },
                sender,
            )
            .await?;
        Ok(())
    }

    /// Handles the end of a turn: the host gets the last word.
    ///
    /// A message the host was holding keeps the turn alive, so the turn ends
    /// only when the host has nothing left to say.
    async fn end_of_turn(
        &self,
        session: &mut Session,
        sender: &mpsc::Sender<EngineEvent>,
        last: Option<Message>,
        head_message: &Message,
    ) -> Result<Option<TurnOutcome>> {
        let history = self.store.history(session.head).await?;
        let consultation = self.consult(session, sender, Some(&history)).await?;
        *session = consultation.session;
        if consultation.added {
            return Ok(None);
        }
        Ok(Some(TurnOutcome {
            session: session.clone(),
            message: last.or_else(|| Some(head_message.clone())),
        }))
    }

    /// Prepares the next request.
    ///
    /// The host may add messages to the session first. An interceptor reads
    /// the history the request carries, so the store is read once and the
    /// request reuses it.
    async fn prepare_request(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<Option<(Session, NodeId, Vec<Message>)>> {
        let before = session.head;
        let history = if self.interceptors.is_empty() {
            None
        } else {
            Some(self.store.history(before).await?)
        };
        let consultation = self.consult(session, sender, history.as_deref()).await?;
        let session = consultation.session;
        if consultation.stop.is_some() {
            return Ok(None);
        }
        let Some(head) = session.head else {
            return Ok(None);
        };
        let history = match history {
            Some(previous) if session.head == before => previous,
            _ => self.store.history(Some(head)).await?,
        };
        Ok(Some((session, head, history)))
    }

    /// Asks the host interceptors what the turn does next, and applies the
    /// messages they add.
    ///
    /// The engine is the only writer while a turn runs, so the commit carries
    /// the version the turn already holds: an interceptor never races the
    /// turn, and what it adds lands in the graph, in the request digest and in
    /// the history the policies read.
    async fn consult(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        history: Option<&[Message]>,
    ) -> Result<Consultation> {
        if self.interceptors.is_empty() {
            return Ok(Consultation::unchanged(session.clone()));
        }
        let sink = ChannelSink {
            sender: sender.clone(),
        };
        let context = InterceptionContext {
            session: session.id,
            turn: session.turn,
            head: session.head,
            phase: session.phase,
            messages: history.unwrap_or_default(),
            events: &sink,
        };
        let mut messages = Vec::new();
        let mut stop = None;
        for interceptor in self.interceptors.interceptors() {
            match interceptor.intercept(&context).await? {
                Interception::Continue => {}
                Interception::Inject(added) => messages.extend(added),
                // `Interception` is `#[non_exhaustive]`: a decision this build
                // does not know is not a licence to run the turn as if the host
                // had said nothing, so it stops like an explicit `Stop`.
                Interception::Stop { reason } => {
                    // The host ended the turn on purpose; say so, so the client
                    // does not read it as a failure.
                    self.emit(
                        sender,
                        EngineEvent::TurnStopped {
                            session: session.id,
                            interceptor: interceptor.name().to_owned(),
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    stop = Some(reason);
                }
                // A decision added to the core after this build: stop, like an
                // explicit `Stop`, and name the interceptor so the client can
                // tell where it came from.
                other => {
                    let reason = format!(
                        "interceptor returned a decision this harness does not understand: {other:?}"
                    );
                    self.emit(
                        sender,
                        EngineEvent::TurnStopped {
                            session: session.id,
                            interceptor: interceptor.name().to_owned(),
                            reason: reason.clone(),
                        },
                    )
                    .await;
                    stop = Some(reason);
                }
            }
        }
        if messages.is_empty() {
            return Ok(Consultation {
                session: session.clone(),
                added: false,
                stop,
            });
        }
        let count = messages.len();
        let mut parent = session.head;
        let mut nodes = Vec::with_capacity(count);
        for message in messages {
            let node = Node::message(message, parent);
            parent = Some(node.id);
            nodes.push(node);
        }
        let committed = self
            .commit(
                Commit {
                    session_id: session.id,
                    expected_version: Some(session.version),
                    nodes,
                    head: parent,
                    phase: session.phase,
                    turn: session.turn,
                    grants: session.grants.clone(),
                    metadata: None,
                },
                sender,
            )
            .await?;
        self.emit(
            sender,
            EngineEvent::InterceptionApplied {
                session: session.id,
                messages: count,
            },
        )
        .await;
        Ok(Consultation {
            session: committed,
            added: true,
            stop,
        })
    }

    /// Chains the turn's input onto the head.
    async fn append_turn(
        &self,
        session: &Session,
        message: Message,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<Session> {
        let node = Node::message(message, session.head);
        let head = node.id;
        let nodes = vec![node];

        let previous_phase = session.phase;
        let committed = self
            .commit(
                Commit {
                    session_id: session.id,
                    expected_version: Some(session.version),
                    nodes,
                    head: Some(head),
                    phase: Phase::Streaming,
                    turn: session.turn + 1,
                    grants: session.grants.clone(),
                    metadata: None,
                },
                sender,
            )
            .await?;
        self.emit(
            sender,
            EngineEvent::PhaseChanged {
                session: session.id,
                from: previous_phase,
                to: Phase::Streaming,
            },
        )
        .await;
        Ok(committed)
    }
}
