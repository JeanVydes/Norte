use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalRecord, ApprovalStatus, GrantScope};
use crate::capability::{Capabilities, ReasoningRequest};
use crate::domain::commit::Commit;
use crate::domain::continuity::{ContinuityToken, ReplayDecision};
use crate::domain::conversation::{CONVERSATION_SCHEMA_VERSION, Conversation};
use crate::domain::media::{MediaPart, MediaSource};
use crate::domain::message::{FinishReason, Message, MessageOrigin, MessageStatus, Usage};
use crate::domain::node::Node;
use crate::domain::part::{Part, ToolCallPart, ToolResultPart};
use crate::domain::role::Role;
use crate::domain::session::{Phase, Session};
use crate::error::{Error, Result};
use crate::event::{EngineEvent, EventSink, StreamDelta};
use crate::id::{ApprovalId, CallId, Digest, GenerationId, ModelRef, NodeId, SessionId, ToolName};
use crate::layer::{LayerContext, LayerStack, ModelContinuation};
use crate::media_adapter::MediaAdapter;
use crate::model::{
    GenerationParams, Model, ModelRequest, ModelStream, SESSION_EXTENSION, StreamLimits,
    TURN_OPTIONS_KEY, TurnOptions,
};
use crate::observer::{Observer, ObserverSet, StreamTransform};
use crate::policy::{PolicyOutcome, PolicySet, PolicyView, combine};
use crate::preprocess::{MediaPayload, MediaPreprocessor};
use crate::store::{BlobStore, Store};
use crate::stream::{Accumulated, StreamAccumulator, StreamEvent};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolContext, ToolOutput, ToolSpec};

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub reasoning: ReasoningRequest,
    pub params: GenerationParams,
    pub limits: StreamLimits,
    /// Media above this many bytes is moved to the blob store.
    pub inline_threshold: usize,
    pub approval_ttl: Duration,
    pub max_iterations: usize,
    pub auto_execute_tools: bool,
    /// When `true` (and `auto_execute_tools` is also `true`), a tool call whose
    /// arguments complete mid-stream begins executing before the assistant
    /// message finishes, subject to the host's tool policy (FR-STREAM-4).
    ///
    /// The policy is evaluated against the assistant message as known at that
    /// point, so a host enables this only when its tool policies do not depend
    /// on the message tail. Calls that need approval, are denied or are rate
    /// limited are always deferred to the normal path. Defaults to `false`.
    pub early_tool_execution: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            reasoning: ReasoningRequest::default(),
            params: GenerationParams::default(),
            limits: StreamLimits::default(),
            inline_threshold: 64 * 1024,
            approval_ttl: Duration::from_secs(10 * 60),
            max_iterations: 16,
            auto_execute_tools: true,
            early_tool_execution: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TurnOutcome {
    pub session: Session,
    pub message: Option<Message>,
}

/// The reconstruction output: the wire-ready message sequence and the positions
/// where the provider may cache the prefix (FR-RECON-7).
#[derive(Clone, Debug)]
struct Reconstructed {
    messages: Vec<Message>,
    cache_boundaries: Vec<u32>,
}

/// Materializes every top-level media part through the adapter. Deterministic by
/// contract so the cached prefix does not move (FR-RECON-4).
async fn materialize_message(
    adapter: &Arc<dyn MediaAdapter>,
    blobs: &dyn BlobStore,
    mut message: Message,
) -> Result<Message> {
    let mut parts = Vec::with_capacity(message.parts.len());
    for part in message.parts {
        match part {
            Part::Media(media) => {
                let media = adapter.resolve(media, blobs).await?;
                parts.push(Part::Media(media));
            }
            other => parts.push(other),
        }
    }
    message.parts = parts;
    Ok(message)
}

/// The effective request options a session carries from a previous turn or an
/// import (FR-EXP-7). A missing or unreadable value means "no session defaults".
fn session_turn_options(session: &Session) -> TurnOptions {
    session
        .metadata
        .get(TURN_OPTIONS_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// Session metadata with the effective request options of the current turn
/// (FR-EXP-7), preserving every host key.
fn turn_metadata(session: &Session, effective: &TurnOptions) -> BTreeMap<String, Value> {
    let mut metadata = session.metadata.clone();
    if let Ok(value) = serde_json::to_value(effective) {
        metadata.insert(TURN_OPTIONS_KEY.to_owned(), value);
    }
    metadata
}

/// Deterministic cache boundary positions, as exclusive end indices into the
/// message sequence: after the leading System/Developer run and at the end of
/// the critical prefix. Never depends on the tail (FR-RECON-7).
fn cache_boundaries(messages: &[Message]) -> Vec<u32> {
    let Ok(len) = u32::try_from(messages.len()) else {
        return Vec::new();
    };
    if len == 0 {
        return Vec::new();
    }
    let mut boundaries = Vec::new();
    let prefix = messages
        .iter()
        .take_while(|message| matches!(message.role, Role::System | Role::Developer))
        .count();
    if let Ok(prefix) = u32::try_from(prefix) {
        if prefix > 0 && prefix < len {
            boundaries.push(prefix);
        }
    }
    boundaries.push(len);
    boundaries
}

/// The pure state machine over a session.
///
/// It holds configuration only; every piece of durable state lives in the
/// store. One transition produces exactly one atomic commit.
#[derive(Clone)]
pub struct Engine {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    model: Arc<dyn Model>,
    tools: Arc<ToolRegistry>,
    policies: Arc<PolicySet>,
    layers: Arc<LayerStack>,
    observers: Arc<ObserverSet>,
    config: Arc<EngineConfig>,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

impl Engine {
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    #[must_use]
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    #[must_use]
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Creates an empty session (extension metadata records the registered
    /// observers and layers so a run stays reproducible).
    ///
    /// # Errors
    ///
    /// Propagates the store error when the initial commit fails.
    pub async fn create(&self) -> Result<Session> {
        let mut session = Session::new();
        session.metadata.insert(
            "observers".to_owned(),
            serde_json::json!(self.observers.names()),
        );
        session.metadata.insert(
            "layers".to_owned(),
            serde_json::json!(
                self.layers
                    .layers()
                    .iter()
                    .map(|layer| layer.name().to_owned())
                    .collect::<Vec<_>>()
            ),
        );
        session.metadata.insert(
            "transforms".to_owned(),
            serde_json::json!(self.observers.transform_names()),
        );
        let commit = Commit {
            session_id: session.id,
            expected_version: None,
            nodes: Vec::new(),
            head: None,
            phase: Phase::Idle,
            turn: 0,
            queue: VecDeque::default(),
            grants: Vec::new(),
            metadata: Some(session.metadata.clone()),
        };
        let session = self.store.commit(commit).await?;
        self.observers.emit(&EngineEvent::SessionCreated {
            session: session.id,
            phase: session.phase,
        });
        Ok(session)
    }

    /// Loads the current session state.
    ///
    /// # Errors
    ///
    /// Returns the store's error, including [`Error::SessionNotFound`].
    pub async fn session(&self, session: SessionId) -> Result<Session> {
        self.store.load(session).await
    }

    /// Loads the ancestor conversation for the session head.
    ///
    /// # Errors
    ///
    /// Propagates the store error while walking the graph.
    pub async fn history(&self, session: SessionId) -> Result<Vec<Message>> {
        let session = self.store.load(session).await?;
        self.store.history(session.head).await
    }

    /// Lists the host approvals still relevant to the session.
    ///
    /// # Errors
    ///
    /// Propagates the store error.
    pub async fn pending_approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>> {
        self.store.pending_approvals(session).await
    }

    /// Enqueues a steering message, drained at the next safe point.
    ///
    /// # Errors
    ///
    /// Returns [`Error::StoreConflict`] when the session advanced under us.
    pub async fn steer(&self, session: SessionId, message: Message) -> Result<Session> {
        let mut current = self.store.load(session).await?;
        current.queue.push_back(message);
        let commit = Commit {
            session_id: current.id,
            expected_version: Some(current.version),
            nodes: Vec::new(),
            head: None,
            phase: current.phase,
            turn: current.turn,
            queue: current.queue.clone(),
            grants: current.grants.clone(),
            metadata: None,
        };
        self.store.commit(commit).await
    }

    /// Appends `messages` to the session as durable history, in order, without
    /// generating (FR-ENG-6).
    ///
    /// This is the array-to-graph bridge: a host that already holds a
    /// conversation as an ordered `Vec<Message>` — a transcript it loaded,
    /// imported from another system or built in memory — persists it verbatim
    /// onto the current head. Nothing is sent to the model, so the next
    /// [`send`](Engine::send) sees the seeded prefix unchanged and the provider
    /// prompt cache keeps hitting (FR-RECON-2).
    ///
    /// Appending requires an `Idle` session: a running turn owns the head, and
    /// moving it underneath the turn would race its commit. An empty slice is a
    /// no-op that returns the session unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the session is not `Idle`, and
    /// propagates the store error, including [`Error::StoreConflict`] when the
    /// session advanced under the caller (FR-ENG-2).
    pub async fn append_history(
        &self,
        session: SessionId,
        messages: Vec<Message>,
    ) -> Result<Session> {
        let current = self.store.load(session).await?;
        if messages.is_empty() {
            return Ok(current);
        }
        if current.phase != Phase::Idle {
            return Err(Error::InvalidInput(format!(
                "append_history requires an Idle session, found {:?}",
                current.phase
            )));
        }

        let mut parent = current.head;
        let mut nodes = Vec::with_capacity(messages.len());
        for message in messages {
            let node = Node::message(message, parent);
            parent = Some(node.id);
            nodes.push(node);
        }
        let appended = nodes.len();
        let committed = self
            .store
            .commit(Commit {
                session_id: current.id,
                expected_version: Some(current.version),
                nodes,
                head: parent,
                phase: current.phase,
                turn: current.turn,
                queue: current.queue.clone(),
                grants: current.grants.clone(),
                metadata: None,
            })
            .await?;
        self.observers.emit(&EngineEvent::HistoryAppended {
            session: committed.id,
            messages: appended,
        });
        Ok(committed)
    }

    /// Exports a session and its ordered history as a self-contained,
    /// serializable [`Conversation`] (FR-EXP-1).
    ///
    /// The envelope carries the session id on purpose: adapters that pass a
    /// session-derived cache key derive it from that id, so re-importing under
    /// the same id keeps the provider cache warm (FR-EXP-2).
    ///
    /// # Errors
    ///
    /// Propagates the store error, including [`Error::SessionNotFound`].
    pub async fn export(&self, session: SessionId) -> Result<Conversation> {
        let state = self.store.load(session).await?;
        let messages = self.store.history(state.head).await?;
        let mut metadata = state.metadata;
        let options = metadata
            .remove(TURN_OPTIONS_KEY)
            .and_then(|value| serde_json::from_value(value).ok());
        Ok(Conversation {
            schema_version: CONVERSATION_SCHEMA_VERSION,
            session: state.id,
            phase: state.phase,
            turn: state.turn,
            metadata,
            queue: state.queue,
            options,
            messages,
        })
    }

    /// Commits a [`Conversation`] as a brand-new session under the same id, so
    /// the next generation reproduces the exact same wire bytes (FR-EXP-2).
    ///
    /// The messages are committed verbatim as history (no generation), chained
    /// onto the new session's head. The store rejects the import when the id
    /// already exists, so a restore never clobbers a live session. Importing
    /// under a different id than the source is an explicit cache-invalidating
    /// fork; the caller decides that by editing [`Conversation::session`]
    /// first.
    ///
    /// Byte-identity to what was previously sent holds when the importing
    /// engine is configured with the same model, capabilities, tools and
    /// generation parameters: reconstruction depends only on those plus the
    /// history, and the history is preserved losslessly.
    ///
    /// # Errors
    ///
    /// Returns [`Error::SchemaVersion`] for an envelope this build does not
    /// understand, and propagates the store error, including
    /// [`Error::StoreConflict`] when the id already exists.
    pub async fn import(&self, conversation: Conversation) -> Result<Session> {
        if conversation.schema_version != CONVERSATION_SCHEMA_VERSION {
            return Err(Error::SchemaVersion {
                kind: "conversation".to_owned(),
                found: conversation.schema_version,
            });
        }

        let mut parent = None;
        let mut nodes = Vec::with_capacity(conversation.messages.len());
        for message in conversation.messages {
            let node = Node::message(message, parent);
            parent = Some(node.id);
            nodes.push(node);
        }

        let mut metadata = conversation.metadata;
        if let Some(options) = &conversation.options {
            if let Ok(value) = serde_json::to_value(options) {
                metadata.insert(TURN_OPTIONS_KEY.to_owned(), value);
            }
        }

        let committed = self
            .store
            .commit(Commit {
                session_id: conversation.session,
                expected_version: None,
                nodes,
                head: parent,
                phase: conversation.phase,
                turn: conversation.turn,
                queue: conversation.queue,
                grants: Vec::new(),
                metadata: Some(metadata),
            })
            .await?;
        Ok(committed)
    }

    /// Starts a turn with a new user message, using the inherited options.
    #[must_use]
    pub fn send(&self, session: SessionId, message: Message) -> EngineStream {
        self.turn(session).input(message).start()
    }

    /// Continues a session from its durable phase, using the inherited options.
    #[must_use]
    pub fn resume(&self, session: SessionId) -> EngineStream {
        self.turn(session).start()
    }

    /// Opens the fluent builder for one turn (FR-ENG-7, FR-ENG-8).
    #[must_use]
    pub fn turn(&self, session: SessionId) -> TurnBuilder<'_> {
        TurnBuilder::new(self, session)
    }

    pub(crate) fn spawn_with(
        &self,
        session: SessionId,
        input: Option<Message>,
        options: TurnOptions,
    ) -> EngineStream {
        let (sender, receiver) = mpsc::channel(self.config.limits.max_buffered_events);
        let cancellation = CancellationToken::new();
        let engine = self.clone();
        let token = cancellation.clone();
        let task =
            tokio::spawn(async move { engine.run(session, input, options, sender, token).await });
        EngineStream {
            receiver,
            task: Some(task),
            cancellation,
        }
    }

    /// The engine-level defaults a turn inherits (FR-ENG-7).
    fn default_turn_options(&self) -> TurnOptions {
        TurnOptions {
            params: self.config.params.clone(),
            reasoning: Some(self.config.reasoning),
            tools: None,
        }
    }

    /// The tool specs to expose for a turn: every registered tool when the turn
    /// does not restrict them, otherwise the named subset in the registry's
    /// canonical order (FR-ENG-8).
    fn effective_tools(&self, options: &TurnOptions) -> Result<Vec<ToolSpec>> {
        let Some(names) = &options.tools else {
            return Ok(self.tools.specs());
        };
        let selected: BTreeSet<&ToolName> = names.iter().collect();
        for name in &selected {
            if self.tools.spec(name).is_none() {
                return Err(Error::ToolNotFound((*name).clone()));
            }
        }
        Ok(self
            .tools
            .specs()
            .into_iter()
            .filter(|spec| selected.contains(&spec.name))
            .collect())
    }

    /// Grants a pending approval and moves the session back to tool execution.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalNotFound`] when no such record exists, or the
    /// store error from saving the grant and the follow-up commit.
    pub async fn approve(
        &self,
        approval: ApprovalId,
        scope: GrantScope,
        actor: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let mut record = self
            .store
            .approval(approval)
            .await?
            .ok_or(Error::ApprovalNotFound(approval))?;
        record.grant(scope, actor);
        self.store.save_approval(record.clone()).await?;

        let session = self.store.load(record.session).await?;
        let mut grants = session.grants.clone();
        grants.extend(record.grants.iter().cloned());
        let commit = Commit {
            session_id: session.id,
            expected_version: Some(session.version),
            nodes: Vec::new(),
            head: None,
            phase: Phase::AwaitingTool,
            turn: session.turn,
            queue: session.queue.clone(),
            grants,
            metadata: None,
        };
        self.store.commit(commit).await?;
        self.observers.emit(&EngineEvent::ApprovalGranted {
            session: record.session,
            approval: record.id,
        });
        Ok(record)
    }

    /// Records a denial and audits it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalNotFound`] when no such record exists, or the
    /// store error from persisting the denial.
    pub async fn deny(
        &self,
        approval: ApprovalId,
        actor: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let mut record = self
            .store
            .approval(approval)
            .await?
            .ok_or(Error::ApprovalNotFound(approval))?;
        record.deny(actor, reason);
        self.store.save_approval(record.clone()).await?;
        Ok(record)
    }

    async fn run(
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

            let head_message = match session.head {
                Some(head) => Some(self.store.node(head).await?.message),
                None => None,
            };
            if let Some(message) = &head_message {
                let pending_tools =
                    session.phase == Phase::AwaitingTool && message.has_tool_calls();
                if message.role == Role::Assistant && !pending_tools {
                    return Ok(TurnOutcome {
                        session,
                        message: last.or_else(|| Some(message.clone())),
                    });
                }
            }

            match session.phase {
                Phase::AwaitingApproval => {
                    return Ok(TurnOutcome {
                        session,
                        message: last,
                    });
                }
                Phase::AwaitingTool => {
                    session = self
                        .run_tools(&session, &sender, &cancellation, &early_results)
                        .await?;
                    early_results.clear();
                }
                Phase::Idle | Phase::Streaming => {
                    let Some(head) = session.head else {
                        return Ok(TurnOutcome {
                            session,
                            message: last,
                        });
                    };
                    let (message, interrupted, pending) = self
                        .generate(&session, head, &effective, &sender, &cancellation)
                        .await?;
                    early_results = pending;
                    let node = Node::message(message.clone(), Some(head));
                    let node_id = node.id;
                    let phase = if interrupted {
                        Phase::Idle
                    } else if message.has_tool_calls() {
                        Phase::AwaitingTool
                    } else {
                        Phase::Idle
                    };
                    session = self
                        .commit(
                            Commit {
                                session_id,
                                expected_version: Some(session.version),
                                nodes: vec![node],
                                head: Some(node_id),
                                phase,
                                turn: session.turn,
                                queue: session.queue.clone(),
                                grants: session.grants.clone(),
                                metadata: Some(turn_metadata(&session, &effective)),
                            },
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
            }
        }
    }

    async fn append_turn(
        &self,
        session: &Session,
        message: Message,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<Session> {
        let mut nodes = Vec::new();
        let mut parent = session.head;
        for steering in &session.queue {
            let node = Node::message(steering.clone(), parent);
            parent = Some(node.id);
            nodes.push(node);
        }
        let node = Node::message(message, parent);
        let head = node.id;
        nodes.push(node);

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
                    queue: VecDeque::default(),
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

    async fn generate(
        &self,
        session: &Session,
        head: NodeId,
        options: &TurnOptions,
        sender: &mpsc::Sender<EngineEvent>,
        cancellation: &CancellationToken,
    ) -> Result<(Message, bool, Vec<ToolResultPart>)> {
        let model_ref = self.model.reference().clone();
        let capabilities = self.model.capabilities();
        let (history, invalidated) = self
            .preprocess_history(self.store.history(Some(head)).await?)
            .await?;
        if invalidated {
            self.emit(
                sender,
                EngineEvent::PrefixInvalidated {
                    session: session.id,
                    reason: "media preprocessing changed the cached prefix".to_owned(),
                },
            )
            .await;
        }
        let reconstructed = self.reconstruct(&history).await?;

        let sent_parts: Vec<Part> = reconstructed
            .messages
            .iter()
            .flat_map(|message| message.parts.iter().cloned())
            .collect();
        capabilities.check_parts(&model_ref, &sent_parts)?;
        let tools = self.effective_tools(options)?;
        capabilities.check_tools(&model_ref, &tools)?;
        let reasoning = options.reasoning.unwrap_or(self.config.reasoning);
        capabilities.check_reasoning(&model_ref, reasoning)?;
        let tool_names: Vec<ToolName> = tools.iter().map(|spec| spec.name.clone()).collect();

        let cache_boundaries = reconstructed.cache_boundaries.clone();
        let request = ModelRequest {
            model: model_ref.clone(),
            messages: reconstructed.messages,
            tools,
            reasoning,
            params: options.params.clone(),
            limits: self.config.limits,
            cache_boundaries: reconstructed.cache_boundaries,
            extensions: BTreeMap::from([(
                SESSION_EXTENSION.to_owned(),
                Value::String(session.id.to_string()),
            )]),
        };
        let request_digest =
            Digest::of_json(&serde_json::to_value(&request).unwrap_or(Value::Null));
        self.emit(
            sender,
            EngineEvent::RequestReconstructed {
                session: session.id,
                turn: session.turn,
                digest: request_digest,
                cache_boundaries,
                params: options.params.clone(),
                reasoning,
                tools: tool_names,
            },
        )
        .await;

        let sink = ChannelSink {
            sender: sender.clone(),
        };
        let context = LayerContext {
            session: session.id,
            turn: session.turn,
            events: &sink,
        };
        let continuation = ModelContinuation {
            model: &*self.model,
        };
        let mut stream = self.layers.run(&context, request, &continuation).await?;
        let (accumulated, interrupted, early_results) = self
            .drain_stream(session, sender, cancellation, &mut stream)
            .await?;

        let parts = self.finalize_output(accumulated.parts).await?;
        let finish_reason = accumulated.finish_reason.unwrap_or(if interrupted {
            FinishReason::Cancelled
        } else {
            FinishReason::Stop
        });
        let status = if interrupted {
            MessageStatus::Interrupted
        } else if matches!(finish_reason, FinishReason::Length) {
            MessageStatus::Truncated
        } else {
            MessageStatus::Complete
        };
        let message = Message::new(Role::Assistant, parts).with_origin(MessageOrigin {
            model: model_ref,
            generation: GenerationId::new(),
            finish_reason,
            usage: accumulated.usage,
            status,
            request_digest: Some(request_digest),
        });
        Ok((message, interrupted, early_results))
    }

    async fn drain_stream(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        cancellation: &CancellationToken,
        stream: &mut ModelStream,
    ) -> Result<(Accumulated, bool, Vec<ToolResultPart>)> {
        let mut accumulator = StreamAccumulator::new(self.config.limits);
        let mut continuity: BTreeMap<u32, ContinuityToken> = BTreeMap::new();
        let mut interrupted = false;
        let mut completed: Vec<Part> = Vec::new();
        let mut early: Vec<(CallId, JoinHandle<ToolResultPart>)> = Vec::new();

        loop {
            let event = tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    interrupted = true;
                    break;
                }
                next = stream.next() => match next {
                    Some(Ok(event)) => event,
                    Some(Err(error)) => {
                        self.emit(sender, EngineEvent::Error {
                            session: Some(session.id),
                            message: error.to_string(),
                        }).await;
                        return Err(error);
                    }
                    None => break,
                }
            };

            if let StreamEvent::Continuity { index, fragment } = &event {
                let merged = self
                    .model
                    .continuity()
                    .merge(continuity.get(index).cloned(), fragment.clone())?;
                continuity.insert(*index, merged.clone());
                accumulator.attach_continuity(*index, merged);
                let delta = StreamDelta::Continuity {
                    index: *index,
                    len: fragment.len(),
                };
                if !self.emit_delta(sender, session, delta).await {
                    interrupted = true;
                    break;
                }
                continue;
            }

            if let Some(part) = accumulator.apply(event.clone())? {
                let delivered = self
                    .emit(
                        sender,
                        EngineEvent::PartCompleted {
                            session: session.id,
                            turn: session.turn,
                            part: part.clone(),
                        },
                    )
                    .await;
                if !delivered {
                    interrupted = true;
                    break;
                }
                let ready = match &part {
                    Part::ToolCall(call) => Some(call.clone()),
                    _ => None,
                };
                completed.push(part);
                if let Some(call) = ready {
                    if let Some(handle) = self.dispatch_early(session, sender, &call, &completed) {
                        early.push((call.call_id, handle));
                    }
                }
            }
            if let Some(delta) = stream_delta(&event) {
                if !self.emit_delta(sender, session, delta).await {
                    interrupted = true;
                    break;
                }
            }
        }

        let mut results = Vec::with_capacity(early.len());
        for (call_id, handle) in early {
            match handle.await {
                Ok(result) => results.push(result),
                Err(error) => results.push(ToolResultPart::error(
                    call_id,
                    format!("early tool task failed: {error}"),
                )),
            }
        }
        Ok((accumulator.finish()?, interrupted, results))
    }

    /// Dispatches a tool call whose arguments just completed for execution before
    /// the assistant message finishes, when the host enabled early tool execution
    /// and the tool policy allows it (FR-STREAM-4).
    ///
    /// The policy is evaluated against the assistant message as known so far.
    /// Calls that need approval, are denied or are rate limited return `None` and
    /// take the normal path.
    fn dispatch_early(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        call: &ToolCallPart,
        completed: &[Part],
    ) -> Option<JoinHandle<ToolResultPart>> {
        if !(self.config.early_tool_execution && self.config.auto_execute_tools) {
            return None;
        }
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            return None;
        };
        self.tools.spec(&tool_name)?;
        let message = Message::new(Role::Assistant, completed.to_vec());
        let view = PolicyView {
            session,
            model: self.model.reference(),
            capabilities: self.model.capabilities(),
            call,
            message: &message,
            prior: &[],
        };
        let arguments = match combine(&self.policies.evaluate(&view)) {
            PolicyOutcome::Allow => call.arguments.clone(),
            PolicyOutcome::Transform { arguments, .. } => arguments,
            PolicyOutcome::Deny { .. }
            | PolicyOutcome::RateLimit { .. }
            | PolicyOutcome::RequireApproval { .. } => return None,
        };
        Some(tokio::spawn(execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            sender.clone(),
            CallSite {
                session: session.id,
                turn: session.turn,
            },
            call.clone(),
            tool_name,
            arguments,
        )))
    }

    /// The reconstructed request: the message sequence plus deterministic cache
    /// boundary positions (FR-RECON-7).
    async fn reconstruct(&self, history: &[Message]) -> Result<Reconstructed> {
        let target = self.model.reference().clone();
        let mut critical = Vec::with_capacity(history.len());
        let mut ephemeral = Vec::new();
        for message in history {
            let mut rebuilt = Vec::with_capacity(message.parts.len());
            for part in &message.parts {
                rebuilt.push(self.replay_part(&target, history, part.clone())?);
            }
            let mut message = message.clone();
            message.parts = rebuilt;
            if let Some(adapter) = &self.media_adapter {
                message = materialize_message(adapter, &*self.blobs, message).await?;
            }
            if message.cache.is_ephemeral() {
                ephemeral.push(message);
            } else {
                critical.push(message);
            }
        }
        let cache_boundaries = cache_boundaries(&critical);
        critical.extend(ephemeral);
        Ok(Reconstructed {
            messages: critical,
            cache_boundaries,
        })
    }

    fn replay_part(&self, target: &ModelRef, history: &[Message], part: Part) -> Result<Part> {
        let Some(token) = part.continuity().cloned() else {
            return Ok(part);
        };
        match self.model.continuity().replay(target, history, &token) {
            ReplayDecision::Replay => Ok(part),
            ReplayDecision::Drop => Ok(part.without_continuity()),
            ReplayDecision::Downgrade(replacement) => Ok(*replacement),
            ReplayDecision::Block(reason) => Err(Error::ContinuityBlocked(reason)),
        }
    }

    /// Applies output ingestion (URLs and vendor files become blobs) and then
    /// externalizes over-threshold inline bytes.
    async fn finalize_output(&self, parts: Vec<Part>) -> Result<Vec<Part>> {
        let mut parts = parts;
        self.preprocess_parts(&mut parts).await?;
        if let Some(adapter) = &self.media_adapter {
            let mut ingested = Vec::with_capacity(parts.len());
            for part in parts {
                match part {
                    Part::Media(media) => {
                        let media = adapter.ingest(media, &*self.blobs).await?;
                        ingested.push(Part::Media(media));
                    }
                    other => ingested.push(other),
                }
            }
            parts = ingested;
        }
        self.externalize(parts).await
    }

    async fn externalize(&self, parts: Vec<Part>) -> Result<Vec<Part>> {
        let mut externalized = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                Part::Media(media) => {
                    if let MediaSource::Inline(bytes) = &media.source {
                        if bytes.len() > self.config.inline_threshold {
                            let id = self.blobs.put(bytes).await?;
                            externalized.push(Part::Media(MediaPart {
                                source: MediaSource::Blob(id),
                                ..media
                            }));
                            continue;
                        }
                    }
                    externalized.push(Part::Media(media));
                }
                other => externalized.push(other),
            }
        }
        Ok(externalized)
    }

    async fn preprocess_history(&self, history: Vec<Message>) -> Result<(Vec<Message>, bool)> {
        if self.media.is_none() {
            return Ok((history, false));
        }
        let mut processed = Vec::with_capacity(history.len());
        let mut changed = false;
        for mut message in history {
            changed |= self.preprocess_parts(&mut message.parts).await?;
            processed.push(message);
        }
        Ok((processed, changed))
    }

    /// Applies the optional, idempotent preprocessing step to the inline media in
    /// `parts`, returning whether any bytes or media type changed: a change after
    /// the prefix was already sent is a cache invalidation the engine reports
    /// (FR-EVT-4). Preprocessing runs on input history and on generated output
    /// alike (FR-PART-3).
    async fn preprocess_parts(&self, parts: &mut [Part]) -> Result<bool> {
        let Some(preprocessor) = &self.media else {
            return Ok(false);
        };
        let mut changed = false;
        for part in parts {
            let Part::Media(media) = part else {
                continue;
            };
            let MediaSource::Inline(bytes) = &media.source else {
                continue;
            };
            let payload = MediaPayload::new(media.kind, media.mime.clone(), bytes.clone());
            let transformed = preprocessor.preprocess(payload).await?;
            if transformed.bytes != *bytes || transformed.mime != media.mime {
                changed = true;
            }
            media.mime = transformed.mime;
            media.source = MediaSource::Inline(transformed.bytes);
        }
        Ok(changed)
    }

    async fn run_tools(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        _cancellation: &CancellationToken,
        early: &[ToolResultPart],
    ) -> Result<Session> {
        let Some(head) = session.head else {
            return Ok(session.clone());
        };
        let head_node = self.store.node(head).await?;
        let calls: Vec<ToolCallPart> = head_node
            .message
            .tool_calls()
            .into_iter()
            .cloned()
            .collect();
        if calls.is_empty() {
            return Ok(session.clone());
        }

        let capabilities = self.model.capabilities();
        let mut results: Vec<ToolResultPart> = Vec::with_capacity(calls.len());

        for call in &calls {
            if let Some(result) = early.iter().find(|result| result.call_id == call.call_id) {
                results.push(result.clone());
                continue;
            }
            if let Some(parked) = self
                .run_one_tool(
                    session,
                    sender,
                    &head_node.message,
                    call,
                    capabilities,
                    &mut results,
                )
                .await?
            {
                return Ok(parked);
            }
        }

        let node = Node::message(Message::tool(results), Some(head));
        let node_id = node.id;
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: vec![node],
                head: Some(node_id),
                phase: Phase::Idle,
                turn: session.turn,
                queue: session.queue.clone(),
                grants: session.grants.clone(),
                metadata: None,
            },
            sender,
        )
        .await
    }

    async fn run_one_tool(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        message: &Message,
        call: &ToolCallPart,
        capabilities: &Capabilities,
        results: &mut Vec<ToolResultPart>,
    ) -> Result<Option<Session>> {
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            results.push(ToolResultPart::error(
                call.call_id,
                format!("invalid tool name: {}", call.name),
            ));
            return Ok(None);
        };

        let view = PolicyView {
            session,
            model: self.model.reference(),
            capabilities,
            call,
            message,
            prior: &[],
        };
        let arguments = match combine(&self.policies.evaluate(&view)) {
            PolicyOutcome::Deny { reason, .. } => {
                results.push(ToolResultPart::error(call.call_id, reason.clone()));
                self.finish_call(session, sender, call, &tool_name, true, 0)
                    .await;
                return Ok(None);
            }
            PolicyOutcome::RateLimit { retry_after, .. } => {
                results.push(ToolResultPart::error(
                    call.call_id,
                    format!("rate limited; retry after {retry_after:?}"),
                ));
                self.finish_call(session, sender, call, &tool_name, true, 0)
                    .await;
                return Ok(None);
            }
            PolicyOutcome::Transform { arguments, .. } => arguments,
            PolicyOutcome::RequireApproval { .. } => {
                if self
                    .authorize(session, call, &tool_name, sender)
                    .await?
                    .is_none()
                {
                    return Ok(Some(self.await_approval(session, sender).await?));
                }
                call.arguments.clone()
            }
            PolicyOutcome::Allow => call.arguments.clone(),
        };

        let result = execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            sender.clone(),
            CallSite {
                session: session.id,
                turn: session.turn,
            },
            call.clone(),
            tool_name,
            arguments,
        )
        .await;
        results.push(result);
        Ok(None)
    }

    async fn finish_call(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        call: &ToolCallPart,
        tool: &ToolName,
        is_error: bool,
        duration_ms: u64,
    ) {
        self.emit(
            sender,
            EngineEvent::ToolCallFinished {
                session: session.id,
                turn: session.turn,
                call_id: call.call_id,
                tool: tool.clone(),
                is_error,
                duration_ms,
            },
        )
        .await;
    }

    async fn authorize(
        &self,
        session: &Session,
        call: &ToolCallPart,
        tool: &ToolName,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<Option<()>> {
        let records = self.store.pending_approvals(session.id).await?;
        if let Some(record) = records.iter().find(|record| record.call_id == call.call_id) {
            if record.is_expired_at(Utc::now()) {
                return Err(Error::ApprovalExpired(record.id));
            }
            if record.status == ApprovalStatus::Denied {
                return Err(Error::ApprovalDenied(
                    "the host denied this call".to_owned(),
                ));
            }
            let granted = session
                .grants
                .iter()
                .any(|grant| grant.is_active() && grant.applies_to(session.id, call.call_id, tool));
            if granted && record.verify_arguments(&call.arguments).is_ok() {
                return Ok(Some(()));
            }
            return Ok(None);
        }

        let record = ApprovalRecord::new(
            session.id,
            call.call_id,
            tool.clone(),
            &call.arguments,
            chrono::Duration::from_std(self.config.approval_ttl)
                .unwrap_or_else(|_| chrono::Duration::hours(1)),
        );
        self.store.save_approval(record.clone()).await?;
        self.emit(
            sender,
            EngineEvent::ApprovalRequested {
                session: session.id,
                approval: record.id,
                call_id: record.call_id,
                tool: record.tool.clone(),
            },
        )
        .await;
        Ok(None)
    }

    async fn await_approval(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
    ) -> Result<Session> {
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: Vec::new(),
                head: None,
                phase: Phase::AwaitingApproval,
                turn: session.turn,
                queue: session.queue.clone(),
                grants: session.grants.clone(),
                metadata: None,
            },
            sender,
        )
        .await
    }

    async fn commit(&self, commit: Commit, sender: &mpsc::Sender<EngineEvent>) -> Result<Session> {
        let session_id = commit.session_id;
        match self.store.commit(commit).await {
            Ok(session) => {
                self.emit(
                    sender,
                    EngineEvent::CommitSucceeded {
                        session: session_id,
                        version: session.version,
                    },
                )
                .await;
                Ok(session)
            }
            Err(
                error @ Error::StoreConflict {
                    expected, actual, ..
                },
            ) => {
                self.emit(
                    sender,
                    EngineEvent::CommitConflict {
                        session: session_id,
                        expected,
                        actual,
                    },
                )
                .await;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    /// Emits one stream delta after running it through the registered transforms
    /// (FR-OBS-2). Returns `false` when the durable channel closed, which the
    /// caller treats as an interruption.
    async fn emit_delta(
        &self,
        sender: &mpsc::Sender<EngineEvent>,
        session: &Session,
        delta: StreamDelta,
    ) -> bool {
        for delta in self.observers.project(delta) {
            let event = EngineEvent::StreamDelta {
                session: session.id,
                turn: session.turn,
                delta,
            };
            if !self.emit(sender, event).await {
                return false;
            }
        }
        true
    }

    async fn emit(&self, sender: &mpsc::Sender<EngineEvent>, event: EngineEvent) -> bool {
        self.observers.emit(&event);
        sender.send(event).await.is_ok()
    }
}

fn into_result(call: &ToolCallPart, output: ToolOutput) -> ToolResultPart {
    let mut parts = output.parts;
    if parts.is_empty() {
        if let Some(structured) = &output.structured {
            parts.push(Part::text(structured.to_string()));
        }
    }
    ToolResultPart {
        call_id: call.call_id,
        parts,
        structured: output.structured,
        is_error: output.is_error,
    }
}

/// The session and turn a tool call belongs to. Grouped so it can be moved into
/// an early-execution task (FR-STREAM-4).
#[derive(Clone, Copy)]
struct CallSite {
    session: SessionId,
    turn: u64,
}

/// Executes one prepared tool call and emits its lifecycle events (FR-TOOL-1).
///
/// It is a free function so it can run inside a spawned task for early tool
/// execution (FR-STREAM-4); [`Engine::run_one_tool`] calls it inline.
async fn execute_call(
    tools: Arc<ToolRegistry>,
    observers: Arc<ObserverSet>,
    sender: mpsc::Sender<EngineEvent>,
    site: CallSite,
    call: ToolCallPart,
    tool: ToolName,
    arguments: Value,
) -> ToolResultPart {
    let started = EngineEvent::ToolCallStarted {
        session: site.session,
        turn: site.turn,
        call_id: call.call_id,
        tool: tool.clone(),
    };
    observers.emit(&started);
    let _ = sender.send(started).await;

    let context = ToolContext::new()
        .with_session(site.session)
        .with_call(call.call_id);
    let clock = Instant::now();
    let output = tools.execute(&context, &tool, arguments).await;
    let duration_ms = u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX);
    let (result, is_error) = match output {
        Ok(output) => {
            let is_error = output.is_error;
            (into_result(&call, output), is_error)
        }
        Err(error) => (
            ToolResultPart {
                call_id: call.call_id,
                parts: vec![Part::text(error.to_string())],
                structured: None,
                is_error: true,
            },
            true,
        ),
    };
    let finished = EngineEvent::ToolCallFinished {
        session: site.session,
        turn: site.turn,
        call_id: call.call_id,
        tool,
        is_error,
        duration_ms,
    };
    observers.emit(&finished);
    let _ = sender.send(finished).await;
    result
}

fn stream_delta(event: &StreamEvent) -> Option<StreamDelta> {
    match event {
        StreamEvent::TextDelta { index, text } => Some(StreamDelta::Text {
            index: *index,
            text: text.clone(),
        }),
        StreamEvent::ReasoningDelta { index, text } => Some(StreamDelta::Reasoning {
            index: *index,
            text: text.clone(),
        }),
        StreamEvent::ToolCallDelta {
            index,
            call_id,
            arguments,
            ..
        } => Some(StreamDelta::ToolArguments {
            index: *index,
            call_id: *call_id,
            arguments: arguments.clone(),
        }),
        StreamEvent::MediaChunk { index, bytes, last } => Some(StreamDelta::Media {
            index: *index,
            bytes: bytes.len(),
            last: *last,
        }),
        StreamEvent::Continuity { index, fragment } => Some(StreamDelta::Continuity {
            index: *index,
            len: fragment.len(),
        }),
        _ => None,
    }
}

struct ChannelSink {
    sender: mpsc::Sender<EngineEvent>,
}

impl EventSink for ChannelSink {
    fn emit(&self, event: EngineEvent) {
        if self.sender.try_send(event).is_err() {
            tracing::debug!("layer event dropped: engine channel full or closed");
        }
    }
}

/// The agent-facing handle on a running turn: a typed event stream plus the
/// final outcome. Dropping it cancels the turn at the next event and commits
/// whatever completed, leaving the session resumable.
pub struct EngineStream {
    receiver: mpsc::Receiver<EngineEvent>,
    task: Option<JoinHandle<Result<TurnOutcome>>>,
    cancellation: CancellationToken,
}

impl EngineStream {
    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    /// Drains the remaining events and returns the outcome.
    ///
    /// # Errors
    ///
    /// Returns the turn error, or [`Error::Other`] if the task panicked.
    pub async fn finish(mut self) -> Result<TurnOutcome> {
        while self.receiver.recv().await.is_some() {}
        let task = self
            .task
            .take()
            .ok_or_else(|| Error::Other("turn already finished".to_owned()))?;
        task.await
            .map_err(|error| Error::Other(format!("engine task failed: {error}")))?
    }
}

impl Stream for EngineStream {
    type Item = Result<EngineEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(event)) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for EngineStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

#[derive(Default)]
pub struct EngineBuilder {
    store: Option<Arc<dyn Store>>,
    blobs: Option<Arc<dyn BlobStore>>,
    model: Option<Arc<dyn Model>>,
    tools: ToolRegistry,
    policies: PolicySet,
    layers: LayerStack,
    observers: Vec<Arc<dyn Observer>>,
    transforms: Vec<Arc<dyn StreamTransform>>,
    config: EngineConfig,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

/// Fluent builder for a single turn (FR-ENG-7, FR-ENG-8).
///
/// It borrows the engine; [`start`](TurnBuilder::start) hands back an
/// [`EngineStream`] and the borrow ends. Every knob is optional and overlays
/// the inherited defaults per field.
pub struct TurnBuilder<'a> {
    engine: &'a Engine,
    session: SessionId,
    input: Option<Message>,
    options: TurnOptions,
}

impl<'a> TurnBuilder<'a> {
    fn new(engine: &'a Engine, session: SessionId) -> Self {
        Self {
            engine,
            session,
            input: None,
            options: TurnOptions::default(),
        }
    }

    /// Sets the user message that starts the turn. Without it the turn resumes
    /// the session from its durable phase.
    #[must_use]
    pub fn input(mut self, message: Message) -> Self {
        self.input = Some(message);
        self
    }

    /// Applies a whole set of overrides at once.
    #[must_use]
    pub fn options(mut self, options: TurnOptions) -> Self {
        self.options = options;
        self
    }

    /// Overrides the generation-parameter delta (merged per field, FR-ENG-7).
    #[must_use]
    pub fn params(mut self, params: GenerationParams) -> Self {
        self.options.params = params;
        self
    }

    #[must_use]
    pub fn temperature(mut self, value: f32) -> Self {
        self.options.params.temperature = Some(value);
        self
    }

    #[must_use]
    pub fn top_p(mut self, value: f32) -> Self {
        self.options.params.top_p = Some(value);
        self
    }

    #[must_use]
    pub fn top_k(mut self, value: u32) -> Self {
        self.options.params.top_k = Some(value);
        self
    }

    #[must_use]
    pub fn max_output_tokens(mut self, value: u64) -> Self {
        self.options.params.max_output_tokens = Some(value);
        self
    }

    #[must_use]
    pub fn stop_sequences(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.params.stop_sequences = values.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn seed(mut self, value: u64) -> Self {
        self.options.params.seed = Some(value);
        self
    }

    #[must_use]
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.options.params.output_schema = Some(schema);
        self
    }

    #[must_use]
    pub fn reasoning(mut self, request: ReasoningRequest) -> Self {
        self.options.reasoning = Some(request);
        self
    }

    /// Restricts the tools exposed to the model for this turn (FR-ENG-8). Names
    /// must exist in the registry; the subset is emitted in canonical order.
    #[must_use]
    pub fn tools(mut self, names: impl IntoIterator<Item = ToolName>) -> Self {
        self.options.tools = Some(names.into_iter().collect());
        self
    }

    /// Spawns the turn and returns its event stream.
    #[must_use]
    pub fn start(self) -> EngineStream {
        self.engine
            .spawn_with(self.session, self.input, self.options)
    }
}

impl EngineBuilder {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn store(mut self, store: Arc<dyn Store>) -> Self {
        self.store = Some(store);
        self
    }

    #[must_use]
    pub fn blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    #[must_use]
    pub fn model(mut self, model: Arc<dyn Model>) -> Self {
        self.model = Some(model);
        self
    }

    #[must_use]
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    #[must_use]
    pub fn policies(mut self, policies: PolicySet) -> Self {
        self.policies = policies;
        self
    }

    #[must_use]
    pub fn layers(mut self, layers: LayerStack) -> Self {
        self.layers = layers;
        self
    }

    #[must_use]
    pub fn observers(mut self, observers: Vec<Arc<dyn Observer>>) -> Self {
        self.observers = observers;
        self
    }

    /// Registers the stream transforms that project deltas for observers
    /// (FR-OBS-2). Their names are recorded in the session metadata (FR-OBS-3).
    #[must_use]
    pub fn transforms(mut self, transforms: Vec<Arc<dyn StreamTransform>>) -> Self {
        self.transforms = transforms;
        self
    }

    /// Adds one stream transform.
    #[must_use]
    pub fn transform(mut self, transform: Arc<dyn StreamTransform>) -> Self {
        self.transforms.push(transform);
        self
    }

    #[must_use]
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Sets an optional media preprocessing step applied to inline input media
    /// before a model call.
    #[must_use]
    pub fn media_preprocessor(mut self, preprocessor: Arc<dyn MediaPreprocessor>) -> Self {
        self.media = Some(preprocessor);
        self
    }

    /// Sets the media materialization port used during reconstruction and on
    /// model output.
    #[must_use]
    pub fn media_adapter(mut self, adapter: Arc<dyn MediaAdapter>) -> Self {
        self.media_adapter = Some(adapter);
        self
    }

    /// Builds the engine, requiring a store, blob store and model.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when a required port is missing.
    pub fn build(self) -> Result<Engine> {
        let store = self
            .store
            .ok_or_else(|| Error::InvalidInput("engine requires a store".to_owned()))?;
        let blobs = self
            .blobs
            .ok_or_else(|| Error::InvalidInput("engine requires a blob store".to_owned()))?;
        let model = self
            .model
            .ok_or_else(|| Error::InvalidInput("engine requires a model".to_owned()))?;
        let buffer = self.config.limits.max_buffered_events;
        Ok(Engine {
            store,
            blobs,
            model,
            tools: Arc::new(self.tools),
            policies: Arc::new(self.policies),
            layers: Arc::new(self.layers),
            observers: Arc::new(ObserverSet::with_transforms(
                self.observers,
                self.transforms,
                buffer,
            )),
            config: Arc::new(self.config),
            media: self.media,
            media_adapter: self.media_adapter,
        })
    }
}

/// Utility for tests and hosts: token accounting of a finished turn.
#[must_use]
pub fn usage_of(message: &Message) -> Usage {
    message
        .origin
        .as_ref()
        .map(|origin| origin.usage.clone())
        .unwrap_or_default()
}
