use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use chrono::Utc;
use futures_core::Stream;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalRecord, ApprovalStatus, AuditAction, GrantScope};
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
use crate::id::{
    ApprovalId, CallId, Digest, GenerationId, GrantId, ModelRef, NodeId, SessionId, ToolName,
};
use crate::interceptor::{Interception, InterceptionContext, InterceptorSet, TurnInterceptor};
use crate::layer::{Continuation, LayerContext, LayerStack};
use crate::media_adapter::MediaAdapter;
use crate::model::{
    GenerationParams, Model, ModelRequest, ModelStream, PARTIAL_TOOLS_KEY, SESSION_EXTENSION,
    StreamLimits, TURN_OPTIONS_KEY, TurnOptions,
};
use crate::observer::{Observer, ObserverSet, StreamTransform};
use crate::policy::{PolicyOutcome, PolicySet, PolicyView, combine};
use crate::preprocess::{MediaPayload, MediaPreprocessor};
use crate::store::{BlobStore, Store};
use crate::stream::{Accumulated, StreamAccumulator, StreamEvent};
use crate::tool::registry::ToolRegistry;
use crate::tool::{ToolCallInfo, ToolContext, ToolMetadataProvider, ToolOutput, ToolSpec};

/// The turn-level policy that is not a layer: what a turn inherits and the
/// bounds the engine enforces.
///
/// The inheritable fields ([`reasoning`](Self::reasoning),
/// [`params`](Self::params)) are defaults, not floors: a turn overlays its own
/// values per field. The rest are bounds the engine applies to every turn.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// The reasoning ask a turn inherits when it does not set one.
    ///
    /// [`ReasoningRequest::Automatic`] by default: the engine asks for whatever
    /// the model does on its own unless the host says otherwise.
    pub reasoning: ReasoningRequest,
    /// The generation parameters a turn inherits.
    ///
    /// A turn merges its own over these per field, so a value set here is the
    /// baseline the turn may override, never a value it cannot change.
    pub params: GenerationParams,
    /// The bounds on buffered observer events and on one stream's size.
    ///
    /// A slow observer never stalls a turn: the observer set drops what does not
    /// fit and counts it. [`StreamLimits::default`] sizes the buffer for
    /// ordinary use.
    pub limits: StreamLimits,
    /// Media above this many bytes is moved to the blob store instead of
    /// travelling inline in the history. 64 KiB by default.
    pub inline_threshold: usize,
    /// How long a parked approval stays decidable before
    /// [`Error::ApprovalExpired`] is the answer.
    ///
    /// Ten minutes by default. The record itself is durable: the clock is what
    /// expires, not the store.
    pub approval_ttl: Duration,
    /// How many requests one turn may spend before it fails with
    /// [`Error::InvalidInput`].
    ///
    /// 16 by default. The count covers every round of the tool loop, and every
    /// request that ran was committed and billed before the error: a cost meter
    /// must keep counting past it. A coding agent that reads, edits and reads
    /// back raises it.
    pub max_iterations: usize,
    /// Whether allowed tool calls run inside the turn.
    ///
    /// `true` by default. When `false`, the turn parks at
    /// [`Phase::AwaitingTool`] and the host runs the
    /// calls and resumes it.
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
    /// The host's per-call tool metadata provider (FR-TOOL-7).
    ///
    /// The engine consults it immediately before every execution, including one
    /// that resumes a parked call, and fills [`ToolContext::metadata`] with the
    /// result. `None` leaves the map empty. The values are execution-scoped:
    /// they are never written to the session, the graph or the wire.
    pub tool_metadata: Option<ToolMetadataProvider>,
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
            tool_metadata: None,
        }
    }
}

/// What a finished turn leaves behind.
#[derive(Clone, Debug)]
pub struct TurnOutcome {
    /// The session as committed.
    pub session: Session,
    /// The assistant message the turn committed, when it committed one.
    pub message: Option<Message>,
}

/// The reconstruction output: the wire-ready message sequence and the positions
/// where the provider may cache the prefix (FR-RECON-7).
#[derive(Clone, Debug)]
struct Reconstructed {
    messages: Vec<Message>,
    cache_boundaries: Vec<u32>,
}

/// Materializes every media part through the adapter, including media nested in
/// a tool result. Deterministic by contract so the cached prefix does not move
/// (FR-RECON-4).
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
            Part::ToolResult(mut result) => {
                let mut inner = Vec::with_capacity(result.parts.len());
                for part in result.parts {
                    match part {
                        Part::Media(media) => {
                            let media = adapter.resolve(media, blobs).await?;
                            inner.push(Part::Media(media));
                        }
                        other => inner.push(other),
                    }
                }
                result.parts = inner;
                parts.push(Part::ToolResult(result));
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
/// (FR-EXP-7), preserving every host key. The parked-results key is dropped:
/// a fresh generate means the previous message's calls are settled.
fn turn_metadata(session: &Session, effective: &TurnOptions) -> BTreeMap<String, Value> {
    let mut metadata = session.metadata.clone();
    metadata.remove(PARTIAL_TOOLS_KEY);
    if let Ok(value) = serde_json::to_value(effective) {
        metadata.insert(TURN_OPTIONS_KEY.to_owned(), value);
    }
    metadata
}

/// The tool results a parked assistant message already produced, in call order.
/// Resuming seeds them so a call that ran before the park is never run twice
/// (FR-APPR-6); an unreadable value is treated as "nothing done yet".
fn partial_results(session: &Session) -> Vec<ToolResultPart> {
    session
        .metadata
        .get(PARTIAL_TOOLS_KEY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// The messages of the request in flight: from the last user message to the end
/// of the history, inclusive. This is what a tool policy sees as `history`, so
/// it can read what this request has already done without a store round trip
/// per call (FR-POL-4).
fn request_messages(history: &[Message]) -> &[Message] {
    let start = history
        .iter()
        .rposition(|message| message.role == Role::User)
        .unwrap_or(0);
    &history[start..]
}

/// The results the calls of one message already have, in the order the message
/// lists them, taking the first source that holds a result for a call.
///
/// Neither a park nor an early dispatch produces results in call order — the
/// first is truncated, the second is shuffled by completion — and a policy reads
/// them the way the message reads (FR-POL-4).
fn results_in_call_order(
    calls: &[ToolCallPart],
    sources: &[&[ToolResultPart]],
) -> Vec<ToolResultPart> {
    calls
        .iter()
        .filter_map(|call| {
            sources
                .iter()
                .find_map(|source| source.iter().find(|result| result.call_id == call.call_id))
                .cloned()
        })
        .collect()
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

/// How a gated call resolved.
enum Authorization {
    /// A matching, verified grant is in force: run the call.
    Granted,
    /// The host said no: the model gets a failed call carrying the reason.
    Denied(String),
    /// The operator has not decided yet: park until they do.
    Pending,
}

/// The reason the host gave when it denied a call, shaped for the model to read.
fn denial_reason(record: &ApprovalRecord) -> String {
    let reason = record
        .audit
        .iter()
        .rev()
        .find(|entry| entry.action == AuditAction::Denied)
        .and_then(|entry| entry.detail.clone())
        .filter(|reason| !reason.trim().is_empty());
    match reason {
        Some(reason) => format!("the operator denied this call: {reason}"),
        None => "the operator denied this call".to_owned(),
    }
}

/// The pure state machine over a session. It holds configuration only; every
/// piece of durable state lives in the store. One transition produces exactly
/// one atomic commit.
#[derive(Clone)]
pub struct Engine {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    model: Arc<dyn Model>,
    tools: Arc<ToolRegistry>,
    policies: Arc<PolicySet>,
    layers: Arc<LayerStack>,
    interceptors: Arc<InterceptorSet>,
    observers: Arc<ObserverSet>,
    config: Arc<EngineConfig>,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

impl Engine {
    /// Starts a fluent builder.
    #[must_use]
    pub fn builder() -> EngineBuilder {
        EngineBuilder::default()
    }

    /// The engine-level configuration.
    #[must_use]
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    /// The registry the engine consults.
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
        session.metadata.insert(
            "interceptors".to_owned(),
            serde_json::json!(self.interceptors.names()),
        );
        let commit = Commit {
            session_id: session.id,
            expected_version: None,
            nodes: Vec::new(),
            head: None,
            phase: Phase::Idle,
            turn: 0,
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

    /// The approvals the host still has to decide for the session: records that
    /// are pending and have not expired, oldest first. Decided and expired
    /// records stay in the store for audit rather than showing up here.
    ///
    /// # Errors
    ///
    /// Propagates the store error.
    pub async fn pending_approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>> {
        let now = Utc::now();
        Ok(self
            .store
            .approvals(session)
            .await?
            .into_iter()
            .filter(|record| record.status == ApprovalStatus::Pending && !record.is_expired_at(now))
            .collect())
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
            extensions: BTreeMap::new(),
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

    /// Revokes a grant and audits it.
    ///
    /// The grant is dropped from the session's standing grants in the same call,
    /// so a tool-, session- or global-scoped grant stops authorizing calls the
    /// moment this returns (FR-APPR-3).
    ///
    /// # Errors
    ///
    /// Returns [`Error::GrantNotFound`] when no record of the session holds that
    /// grant, and propagates store failures.
    pub async fn revoke(
        &self,
        session: SessionId,
        grant: GrantId,
        actor: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let records = self.store.approvals(session).await?;
        let mut record = records
            .into_iter()
            .find(|record| record.grants.iter().any(|held| held.id == grant))
            .ok_or(Error::GrantNotFound(grant))?;
        record.revoke(grant, actor);
        self.store.save_approval(record.clone()).await?;

        let revoked_at = record
            .grants
            .iter()
            .find(|held| held.id == grant)
            .and_then(|held| held.revoked_at);
        let current = self.store.load(session).await?;
        let mut grants = current.grants.clone();
        for held in &mut grants {
            if held.id == grant {
                held.revoked_at = revoked_at;
            }
        }
        self.store
            .commit(Commit {
                session_id: session,
                expected_version: Some(current.version),
                nodes: Vec::new(),
                head: None,
                phase: current.phase,
                turn: current.turn,
                grants,
                metadata: None,
            })
            .await?;
        self.observers.emit(&EngineEvent::ApprovalRevoked {
            session,
            approval: record.id,
            grant,
        });
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

    async fn generate(
        &self,
        session: &Session,
        history: Vec<Message>,
        options: &TurnOptions,
        sender: &mpsc::Sender<EngineEvent>,
        cancellation: &CancellationToken,
    ) -> Result<(Message, bool, Vec<ToolResultPart>)> {
        let model_ref = self.model.reference().clone();
        let capabilities = self.model.capabilities();
        let (history, invalidated) = self.preprocess_history(history).await?;
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
        capabilities.check_generation(&model_ref, &options.params)?;
        let tool_names: Vec<ToolName> = tools.iter().map(|spec| spec.name.clone()).collect();

        let cache_boundaries = reconstructed.cache_boundaries.clone();
        let mut extensions = options.extensions.clone();
        extensions.insert(
            SESSION_EXTENSION.to_owned(),
            Value::String(session.id.to_string()),
        );
        let request = ModelRequest {
            model: model_ref.clone(),
            messages: reconstructed.messages,
            tools,
            reasoning,
            params: options.params.clone(),
            limits: self.config.limits,
            cache_boundaries: reconstructed.cache_boundaries,
            extensions,
        };
        // A layer may alter the request, so the digest must describe the one the
        // model receives (FR-ENG-4). The clone is the fallback for a layer that
        // answers without the model: the request it received is what the digest
        // describes then.
        let reconstructed = request.clone();
        let wire_digest = Mutex::new(None);
        let sink = ChannelSink {
            sender: sender.clone(),
        };
        let context = LayerContext {
            session: session.id,
            turn: session.turn,
            events: &sink,
        };
        let continuation = Digesting {
            model: &*self.model,
            digest: &wire_digest,
        };
        let mut stream = self.layers.run(&context, request, &continuation).await?;
        let request_digest = wire_digest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_else(|| digest_of(&reconstructed));
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
        let (accumulated, interrupted, early_results) = self
            .drain_stream(
                session,
                sender,
                cancellation,
                &mut stream,
                request_messages(&history),
            )
            .await?;

        let message = self
            .finalize_message(accumulated, interrupted, model_ref, request_digest)
            .await?;
        Ok((message, interrupted, early_results))
    }

    /// Assembles the committed assistant message from what the stream
    /// accumulated: its final parts, its finish reason and the provenance a
    /// replay reads back (FR-ENG-9).
    async fn finalize_message(
        &self,
        accumulated: Accumulated,
        interrupted: bool,
        model: ModelRef,
        request_digest: Digest,
    ) -> Result<Message> {
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
        Ok(
            Message::new(Role::Assistant, parts).with_origin(MessageOrigin {
                model,
                generation: GenerationId::new(),
                finish_reason,
                usage: accumulated.usage,
                status,
                request_digest: Some(request_digest),
            }),
        )
    }

    async fn drain_stream(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        cancellation: &CancellationToken,
        stream: &mut ModelStream,
        history: &[Message],
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
                    if let Some(handle) = self.dispatch_early(
                        session,
                        sender,
                        &call,
                        &completed,
                        history,
                        cancellation,
                    ) {
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
        let interrupted = interrupted || cancellation.is_cancelled();
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
    /// The policy is evaluated against the assistant message as known so far,
    /// with no results of its own — nothing has run yet — and the history
    /// committed up to the previous tool turn. Calls that need approval, are
    /// denied or are rate limited return `None` and take the normal path.
    fn dispatch_early(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        call: &ToolCallPart,
        completed: &[Part],
        history: &[Message],
        cancellation: &CancellationToken,
    ) -> Option<JoinHandle<ToolResultPart>> {
        if !(self.config.early_tool_execution && self.config.auto_execute_tools) {
            return None;
        }
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            return None;
        };
        self.tools.spec(&tool_name)?;
        let message = Message::new(Role::Assistant, completed.to_vec());
        let decisions = {
            let view = PolicyView {
                session,
                model: self.model.reference(),
                capabilities: self.model.capabilities(),
                call,
                message: &message,
                prior: &[],
                results: &[],
                history,
            };
            self.policies.evaluate(&view)
        };
        let arguments = match combine(&decisions) {
            PolicyOutcome::Allow => call.arguments.clone(),
            PolicyOutcome::Transform { arguments, .. } => arguments,
            PolicyOutcome::Deny { .. }
            | PolicyOutcome::RateLimit { .. }
            | PolicyOutcome::RequireApproval { .. } => return None,
        };
        let metadata = self.call_metadata(session, call, &tool_name, &arguments);
        Some(tokio::spawn(execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            sender.clone(),
            CallSite {
                session: session.id,
                turn: session.turn,
            },
            PreparedCall {
                call: call.clone(),
                tool: tool_name,
                arguments,
                metadata,
            },
            cancellation.clone(),
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
        cancellation: &CancellationToken,
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
        let history = {
            let full = self.store.history(Some(head)).await?;
            request_messages(&full).to_vec()
        };
        // Everything that already produced a result — during this stream and
        // before a park — seeded in call order, so a policy reads it the way the
        // message lists the calls (FR-POL-4).
        let persisted = partial_results(session);
        let mut results = results_in_call_order(&calls, &[early, persisted.as_slice()]);
        let turn = ToolTurn {
            session,
            sender,
            message: &head_node.message,
            capabilities,
            history: &history,
        };

        for call in &calls {
            if results.iter().any(|result| result.call_id == call.call_id) {
                continue;
            }
            if cancellation.is_cancelled() {
                return Ok(session.clone());
            }
            if let Some(parked) = self
                .run_one_tool(&turn, call, &mut results, cancellation)
                .await?
            {
                return Ok(parked);
            }
            if cancellation.is_cancelled() {
                return Ok(session.clone());
            }
        }

        let settled = results_in_call_order(&calls, &[results.as_slice()]);
        let node = Node::message(Message::tool(settled), Some(head));
        let node_id = node.id;
        let mut metadata = session.metadata.clone();
        metadata.remove(PARTIAL_TOOLS_KEY);
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: vec![node],
                head: Some(node_id),
                phase: Phase::Idle,
                turn: session.turn,
                grants: session.grants.clone(),
                metadata: Some(metadata),
            },
            sender,
        )
        .await
    }

    /// The host metadata for one execution, consulted immediately before the
    /// executor runs so a resumed park is filled like an inline call (FR-TOOL-7).
    fn call_metadata(
        &self,
        session: &Session,
        call: &ToolCallPart,
        tool: &ToolName,
        arguments: &Value,
    ) -> BTreeMap<String, Value> {
        self.config
            .tool_metadata
            .as_ref()
            .map_or_else(BTreeMap::new, |provider| {
                provider.metadata(&ToolCallInfo {
                    session: session.id,
                    turn: session.turn,
                    call_id: call.call_id,
                    tool: tool.clone(),
                    arguments: arguments.clone(),
                })
            })
    }

    async fn run_one_tool(
        &self,
        turn: &ToolTurn<'_>,
        call: &ToolCallPart,
        results: &mut Vec<ToolResultPart>,
        cancellation: &CancellationToken,
    ) -> Result<Option<Session>> {
        let ToolTurn {
            session,
            sender,
            message,
            capabilities,
            history,
        } = *turn;
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            results.push(ToolResultPart::error(
                call.call_id,
                format!("invalid tool name: {}", call.name),
            ));
            return Ok(None);
        };

        let decisions = {
            let view = PolicyView {
                session,
                model: self.model.reference(),
                capabilities,
                call,
                message,
                prior: &[],
                results: results.as_slice(),
                history,
            };
            self.policies.evaluate(&view)
        };
        let arguments = match combine(&decisions) {
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
            PolicyOutcome::RequireApproval {
                arguments: rewritten,
                ..
            } => {
                let arguments = rewritten.unwrap_or_else(|| call.arguments.clone());
                match self
                    .authorize(session, call, &tool_name, sender, &arguments)
                    .await?
                {
                    Authorization::Granted => arguments,
                    Authorization::Pending => {
                        return Ok(Some(
                            self.await_approval(session, sender, results.as_slice())
                                .await?,
                        ));
                    }
                    Authorization::Denied(reason) => {
                        // The operator said no: the model gets that back as a
                        // failed call so it can adapt, exactly like a policy
                        // denial or a tool that errored.
                        results.push(ToolResultPart::error(call.call_id, reason));
                        self.finish_call(session, sender, call, &tool_name, true, 0)
                            .await;
                        return Ok(None);
                    }
                }
            }
            PolicyOutcome::Allow => call.arguments.clone(),
        };

        let metadata = self.call_metadata(session, call, &tool_name, &arguments);
        let result = execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            sender.clone(),
            CallSite {
                session: session.id,
                turn: session.turn,
            },
            PreparedCall {
                call: call.clone(),
                tool: tool_name,
                arguments,
                metadata,
            },
            cancellation.clone(),
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

    /// Resolves the approval state of one gated call.
    ///
    /// A call with no record gets one here, and the host is told. This is what
    /// makes a parked conversation resume: the request is rebuilt from the head
    /// message's tool calls, with a fresh record and a fresh id, rather than
    /// from a ledger that may never have travelled.
    async fn authorize(
        &self,
        session: &Session,
        call: &ToolCallPart,
        tool: &ToolName,
        sender: &mpsc::Sender<EngineEvent>,
        arguments: &Value,
    ) -> Result<Authorization> {
        let records = self.store.approvals(session.id).await?;
        let decided = records.iter().find(|record| record.call_id == call.call_id);
        if let Some(record) = decided {
            if record.is_expired_at(Utc::now()) || record.status == ApprovalStatus::Expired {
                return Err(Error::ApprovalExpired(record.id));
            }
            if record.status == ApprovalStatus::Denied {
                return Ok(Authorization::Denied(denial_reason(record)));
            }
            match record.authorize(Utc::now(), arguments) {
                Ok(()) => return Ok(Authorization::Granted),
                Err(Error::ApprovalDenied(reason)) if record.status == ApprovalStatus::Granted => {
                    // A grant that was revoked, or arguments that no longer
                    // match the digest it was issued for (FR-APPR-4).
                    return Ok(Authorization::Denied(reason));
                }
                Err(Error::ApprovalDenied(_)) => {}
                Err(error) => return Err(error),
            }
        }

        // A standing grant, scoped to the tool, the session or everything,
        // authorizes later calls without asking again (FR-APPR-3). A grant
        // scoped to one call is already covered by the record above.
        let standing = session
            .grants
            .iter()
            .any(|grant| grant.applies_to(session.id, call.call_id, tool));
        if standing {
            return Ok(Authorization::Granted);
        }

        if decided.is_some() {
            return Ok(Authorization::Pending);
        }

        let record = ApprovalRecord::new(
            session.id,
            call.call_id,
            tool.clone(),
            arguments,
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
        Ok(Authorization::Pending)
    }

    async fn await_approval(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        partial: &[ToolResultPart],
    ) -> Result<Session> {
        let mut metadata = session.metadata.clone();
        metadata.remove(PARTIAL_TOOLS_KEY);
        if !partial.is_empty() {
            if let Ok(value) = serde_json::to_value(partial) {
                metadata.insert(PARTIAL_TOOLS_KEY.to_owned(), value);
            }
        }
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: Vec::new(),
                head: None,
                phase: Phase::AwaitingApproval,
                turn: session.turn,
                grants: session.grants.clone(),
                metadata: Some(metadata),
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

/// The invariant context of one tool turn: the session, the event channel, the
/// assistant message carrying the calls, the model's capabilities and the
/// request history a policy reads (FR-POL-4). Grouped so the per-call
/// arguments of the tool loop stay few.
#[derive(Clone, Copy)]
struct ToolTurn<'a> {
    session: &'a Session,
    sender: &'a mpsc::Sender<EngineEvent>,
    message: &'a Message,
    capabilities: &'a Capabilities,
    history: &'a [Message],
}

/// The session and turn a tool call belongs to. Grouped so it can be moved into
/// an early-execution task (FR-STREAM-4).
#[derive(Clone, Copy)]
struct CallSite {
    session: SessionId,
    turn: u64,
}

/// One tool call, past policy: everything `execute_call` needs about it, so the
/// free function keeps a short signature (FR-TOOL-1).
struct PreparedCall {
    call: ToolCallPart,
    tool: ToolName,
    arguments: Value,
    metadata: BTreeMap<String, Value>,
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
    prepared: PreparedCall,
    cancellation: CancellationToken,
) -> ToolResultPart {
    let PreparedCall {
        call,
        tool,
        arguments,
        metadata,
    } = prepared;
    let started = EngineEvent::ToolCallStarted {
        session: site.session,
        turn: site.turn,
        call_id: call.call_id,
        tool: tool.clone(),
    };
    observers.emit(&started);
    let _ = sender.send(started).await;

    let mut context = ToolContext::new()
        .with_session(site.session)
        .with_call(call.call_id)
        .with_cancellation(cancellation);
    for (key, value) in metadata {
        context = context.with_metadata(key, value);
    }
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
        StreamEvent::RefusalDelta { index, text } => Some(StreamDelta::Refusal {
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
        StreamEvent::SafetyRatings { ratings } => Some(StreamDelta::SafetyRatings {
            ratings: ratings.clone(),
        }),
        StreamEvent::Logprobs { index, entries } => Some(StreamDelta::Logprobs {
            index: *index,
            entries: entries.clone(),
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

/// What the host decided, and what the engine did about it.
struct Consultation {
    /// The session after the messages the host added.
    session: Session,
    /// Whether messages joined the session.
    added: bool,
    /// Whether the host asked for the turn to end.
    stop: Option<String>,
}

impl Consultation {
    /// A consultation that changed nothing.
    fn unchanged(session: Session) -> Self {
        Self {
            session,
            added: false,
            stop: None,
        }
    }
}

/// The digest of a request, over its canonical JSON text.
fn digest_of(request: &ModelRequest) -> Digest {
    Digest::of_json(&serde_json::to_value(request).unwrap_or(Value::Null))
}

/// The terminal that reports the request the model actually received.
///
/// A layer may alter the request before it reaches the model, so the digest
/// the generation records is the one of the wire, not of the reconstruction
/// (FR-ENG-4).
struct Digesting<'a> {
    model: &'a dyn Model,
    digest: &'a Mutex<Option<Digest>>,
}

#[async_trait::async_trait]
impl Continuation for Digesting<'_> {
    async fn call(
        &self,
        _context: &LayerContext<'_>,
        request: ModelRequest,
    ) -> Result<ModelStream> {
        *self.digest.lock().unwrap_or_else(PoisonError::into_inner) = Some(digest_of(&request));
        self.model.stream(request).await
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
    /// Asks the turn to stop.
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

/// Fluent builder for an engine.
///
/// Three ports are required — a [`Store`], a [`BlobStore`] and a [`Model`] — and
/// [`build`](EngineBuilder::build) fails with [`Error::InvalidInput`] naming the
/// one that is missing. Everything else has a default: an empty tool registry,
/// no policies, no layers, no observers and [`EngineConfig::default`].
///
/// The ports are built once, at the edge of the program, from configuration; the
/// rest of the application sees [`Engine`] and never a vendor. A knob left unset
/// keeps its default behaviour, so a builder lists only what the host decided.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
///
/// use norte_core::{
///     Capabilities, EngineBuilder, InMemoryBlobStore, InMemoryStore, Message, Modality,
///     ToolRegistry,
/// };
/// use norte_openai::presets::openai;
///
/// let capabilities = Capabilities::builder()
///     .input(Modality::Text)
///     .output(Modality::Text)
///     .build();
///
/// // Built once, at the edge of the program; nothing above the `Model` port
/// // learns which vendor answered.
/// let key = std::env::var("OPENAI_API_KEY").expect("key");
/// let model = Arc::new(openai("gpt-5", key, capabilities).expect("model"));
///
/// let engine = EngineBuilder::new()
///     .store(Arc::new(InMemoryStore::new()))
///     .blobs(Arc::new(InMemoryBlobStore::new()))
///     .model(model)
///     .tools(ToolRegistry::new())
///     .build()
///     .expect("engine");
///
/// let session = engine.create().await.expect("session");
///
/// // The outcome carries the turn's parts, its usage and why it stopped.
/// let outcome = engine
///     .send(session.id, Message::user("Hello!"))
///     .finish()
///     .await
///     .expect("turn");
/// ```
#[derive(Default)]
pub struct EngineBuilder {
    store: Option<Arc<dyn Store>>,
    blobs: Option<Arc<dyn BlobStore>>,
    model: Option<Arc<dyn Model>>,
    tools: ToolRegistry,
    policies: PolicySet,
    layers: LayerStack,
    interceptors: Vec<Arc<dyn TurnInterceptor>>,
    observers: Vec<Arc<dyn Observer>>,
    transforms: Vec<Arc<dyn StreamTransform>>,
    config: EngineConfig,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

/// Fluent builder for a single turn (FR-ENG-7, FR-ENG-8).
///
/// It borrows the engine; [`start`](TurnBuilder::start) hands back an
/// [`EngineStream`] and the borrow ends there. Every knob is optional and
/// overlays the inherited defaults per field: what a turn sets replaces that one
/// field of the effective [`GenerationParams`] and nothing else, so a turn that
/// only changes the temperature keeps the engine's other parameters.
///
/// # Example
///
/// ```ignore
/// use norte_core::{Message, ToolName};
///
/// let outcome = engine
///     .turn(session.id)
///     .input(Message::user("Summarize src/lib.rs"))
///     .temperature(0.2)
///     .max_output_tokens(512)
///     .tools([ToolName::new("read_file").expect("tool")])
///     .start()
///     .finish()
///     .await
///     .expect("turn");
/// ```
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

    /// Applies a whole set of overrides at once. A per-field knob called after
    /// this one wins.
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

    /// Sets the sampling temperature for this turn.
    #[must_use]
    pub fn temperature(mut self, value: f32) -> Self {
        self.options.params.temperature = Some(value);
        self
    }

    /// Sets the nucleus-sampling mass: the most probable tokens whose cumulative
    /// probability reaches this value are the ones sampled from.
    #[must_use]
    pub fn top_p(mut self, value: f32) -> Self {
        self.options.params.top_p = Some(value);
        self
    }

    /// Keeps only the `value` most probable tokens at each step.
    #[must_use]
    pub fn top_k(mut self, value: u32) -> Self {
        self.options.params.top_k = Some(value);
        self
    }

    /// Caps how many tokens the model may produce in this turn.
    #[must_use]
    pub fn max_output_tokens(mut self, value: u64) -> Self {
        self.options.params.max_output_tokens = Some(value);
        self
    }

    /// Stops generation when the model produces any of these sequences. The
    /// sequences themselves are not part of the answer.
    #[must_use]
    pub fn stop_sequences(mut self, values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.options.params.stop_sequences = values.into_iter().map(Into::into).collect();
        self
    }

    /// Fixes the sampling seed, where the vendor honours it.
    ///
    /// The same seed and parameters promise the same sampling, not the same
    /// answer: the model's own state and the provider's version still count.
    #[must_use]
    pub fn seed(mut self, value: u64) -> Self {
        self.options.params.seed = Some(value);
        self
    }

    /// Asks for structured output against this JSON Schema.
    #[must_use]
    pub fn output_schema(mut self, schema: Value) -> Self {
        self.options.params.output_schema = Some(schema);
        self
    }

    /// Requests token-level log probabilities for this turn: `top` runner-up
    /// tokens per position, or `0` for the chosen token's probability alone.
    /// The model must declare
    /// [`Feature::Logprobs`](crate::Feature::Logprobs); the engine rejects the
    /// turn before the request otherwise (FR-LP-1).
    #[must_use]
    pub fn logprobs(mut self, top: u8) -> Self {
        self.options.params.logprobs = Some(top);
        self
    }

    /// Overrides the reasoning ask for this turn.
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

    /// Sets a vendor request extra for this turn, keyed `<wire-family>:<field>`
    /// (Section 12), for example `google.gemini:safetySettings`. A key whose
    /// family does not match the active wire is ignored.
    #[must_use]
    pub fn extension(mut self, key: impl Into<String>, value: Value) -> Self {
        self.options.extensions.insert(key.into(), value);
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
    /// An empty builder: no store, no blob store, no model, and every optional
    /// piece at its default.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the session store: sessions, nodes, messages and approval records.
    ///
    /// Required. [`InMemoryStore`](crate::InMemoryStore) is the reference
    /// implementation; a host whose sessions outlive the process implements
    /// [`Store`] over its own database.
    #[must_use]
    pub fn store(mut self, store: Arc<dyn Store>) -> Self {
        self.store = Some(store);
        self
    }

    /// Sets the blob store that holds media bytes and large tool output by
    /// content hash.
    ///
    /// Required. [`InMemoryBlobStore`](crate::InMemoryBlobStore) is the
    /// reference implementation; the bytes live where the host puts them.
    #[must_use]
    pub fn blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Sets the model that answers.
    ///
    /// Required. One `Arc<dyn Model>` is built at the edge of the program and
    /// everything above the port stays independent of the vendor.
    #[must_use]
    pub fn model(mut self, model: Arc<dyn Model>) -> Self {
        self.model = Some(model);
        self
    }

    /// Sets the tool registry the turn draws declarations from.
    ///
    /// Empty by default: a turn that registers no tool is a plain chat.
    #[must_use]
    pub fn tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    /// Sets the policy set evaluated for every tool call, in order.
    ///
    /// Empty by default, which lets every call that reaches it run.
    #[must_use]
    pub fn policies(mut self, policies: PolicySet) -> Self {
        self.policies = policies;
        self
    }

    /// Sets the layer stack that wraps each model call.
    ///
    /// Empty by default.
    #[must_use]
    pub fn layers(mut self, layers: LayerStack) -> Self {
        self.layers = layers;
        self
    }

    /// Sets the interceptors the engine consults before every model request of
    /// a turn (FR-ENG-10).
    ///
    /// Empty by default. An interceptor may return messages to append to the
    /// session's history; the engine appends them itself, so they are durable
    /// and visible to the policies.
    #[must_use]
    pub fn interceptors(mut self, interceptors: Vec<Arc<dyn TurnInterceptor>>) -> Self {
        self.interceptors = interceptors;
        self
    }

    /// Sets the observers that see every [`EngineEvent`].
    ///
    /// Empty by default. A slow observer never stalls a turn: the set buffers
    /// events and drops what does not fit, and
    /// [`ObserverSet::dropped`](crate::ObserverSet::dropped) counts the drops.
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

    /// Sets the engine-level configuration. Fields left at their default keep
    /// their default behaviour.
    #[must_use]
    pub fn config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Installs the host's per-call tool metadata provider (FR-TOOL-7).
    ///
    /// The callback runs immediately before each tool execution, including one
    /// that resumes a parked call, and its values land in
    /// [`ToolContext::metadata`]. Nothing is persisted.
    #[must_use]
    pub fn tool_metadata(
        mut self,
        provider: impl Fn(&ToolCallInfo) -> BTreeMap<String, Value> + Send + Sync + 'static,
    ) -> Self {
        self.config.tool_metadata = Some(ToolMetadataProvider::new(provider));
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

    /// Validates the three required ports and returns the engine.
    ///
    /// Everything optional is folded in here: the registry, the policy set and
    /// the layer stack are shared behind an `Arc`, and the observers and
    /// transforms become one [`ObserverSet`] bounded by
    /// [`EngineConfig::limits`](crate::EngineConfig::limits).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] naming the port that is missing.
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
            interceptors: Arc::new(InterceptorSet::new(self.interceptors)),
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
