#![forbid(unsafe_code)]
#![doc = "One reference agent harness over Norte's contracts: turns, the tool loop, approvals and streaming.

Norte's contracts live in [`norte_core`]: the [`Model`](norte_core::Model) port, the
[`Store`](norte_core::Store), the [`ToolRegistry`](norte_core::ToolRegistry), the
policies, the layers and the observers. This crate is one way to drive them:
[`Harness`] walks a session forward one turn at a time.

It is deliberately *a* harness and not *the* harness. Nothing in the core refers
to it, nothing forces a host to use it, and a host that wants a different turn
policy — a different loop, its own scheduling, a graph instead of a session —
depends on `norte-core` alone and writes it. What the core owns is the contracts
and the durable state; what this crate owns is one order of operations over them.

```no_run
use norte_harness::{HarnessBuilder, InMemoryBlobStore, InMemoryStore};
# fn model() -> std::sync::Arc<dyn norte_core::Model> { todo!() }
let harness = HarnessBuilder::new()
    .store(std::sync::Arc::new(InMemoryStore::default()))
    .blobs(std::sync::Arc::new(InMemoryBlobStore::default()))
    .model(model())
    .build()
    .expect(\"a harness\");
```

[`Model`]: norte_core::Model
[`Store`]: norte_core::Store
[`ToolRegistry`]: norte_core::ToolRegistry
"]

/// The record of an approval decision, and the grant it issues.
pub mod approval;
/// What the host decides about a call a policy parked.
pub mod approvals;
/// In-memory reference implementations of the ports.
pub mod interceptor;
pub mod memory;
/// The tool-call decision: a rule over one call, evaluated before it runs.
pub mod policy;
/// Building and reconstructing one request.
pub mod request;
/// Reading and committing a session's durable state.
pub mod session;
/// The ports the runtime persists through.
pub mod store;
/// Running the calls a model asked for.
pub mod tools;
/// The turn loop.
pub mod turn;

use std::collections::{BTreeMap, BTreeSet};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use futures_core::Stream;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub use crate::approval::{ApprovalRecord, AuditAction};
pub use crate::interceptor::{Interception, InterceptionContext, InterceptorSet, TurnInterceptor};
pub use crate::policy::{
    Decision, Policy, PolicyDecision, PolicyOutcome, PolicySet, PolicyView, ToolApprovalPolicy,
    combine,
};
pub use crate::store::Store;
use norte_core::BlobStore;
use norte_core::capability::{Capabilities, ReasoningRequest};
use norte_core::domain::commit::Commit;
use norte_core::domain::message::{Message, Usage};
use norte_core::domain::part::{Part, ToolCallPart, ToolResultPart};
use norte_core::domain::role::Role;
use norte_core::domain::session::Session;
use norte_core::error::{Error, Result};
use norte_core::event::{EngineEvent, EventSink, StreamDelta};
use norte_core::id::{Digest, SessionId, ToolName};
use norte_core::layer::{Continuation, LayerContext, LayerStack};
use norte_core::media_adapter::MediaAdapter;
use norte_core::model::{
    GenerationParams, Model, ModelRequest, ModelStream, PARTIAL_TOOLS_KEY, StreamLimits,
    TURN_OPTIONS_KEY, TurnOptions,
};
use norte_core::observer::{Observer, ObserverSet, StreamTransform};
use norte_core::preprocess::MediaPreprocessor;
use norte_core::stream::StreamEvent;
use norte_core::tool::registry::ToolRegistry;
use norte_core::tool::{ToolCallInfo, ToolContext, ToolMetadataProvider, ToolOutput, ToolSpec};

/// The turn-level policy that is not a layer: what a turn inherits and the
/// bounds the engine enforces.
///
/// The inheritable fields ([`reasoning`](Self::reasoning),
/// [`params`](Self::params)) are defaults, not floors: a turn overlays its own
/// values per field. The rest are bounds the engine applies to every turn.
#[derive(Clone, Debug)]
pub struct HarnessConfig {
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

impl Default for HarnessConfig {
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
pub struct Harness {
    store: Arc<dyn Store>,
    blobs: Arc<dyn BlobStore>,
    model: Arc<dyn Model>,
    tools: Arc<ToolRegistry>,
    policies: Arc<PolicySet>,
    layers: Arc<LayerStack>,
    interceptors: Arc<InterceptorSet>,
    observers: Arc<ObserverSet>,
    config: Arc<HarnessConfig>,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

impl Harness {
    /// Starts a fluent builder.
    #[must_use]
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::default()
    }

    /// The engine-level configuration.
    #[must_use]
    pub fn config(&self) -> &HarnessConfig {
        &self.config
    }

    /// The registry the engine consults.
    #[must_use]
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Starts a turn with a new user message, using the inherited options.
    #[must_use]
    pub fn send(&self, session: SessionId, message: Message) -> HarnessStream {
        self.turn(session).input(message).start()
    }

    /// Continues a session from its durable phase, using the inherited options.
    #[must_use]
    pub fn resume(&self, session: SessionId) -> HarnessStream {
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
    ) -> HarnessStream {
        let (sender, receiver) = mpsc::channel(self.config.limits.max_buffered_events);
        let cancellation = CancellationToken::new();
        let engine = self.clone();
        let token = cancellation.clone();
        let task =
            tokio::spawn(async move { engine.run(session, input, options, sender, token).await });
        HarnessStream {
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
/// execution (FR-STREAM-4); the engine's tool loop calls it inline.
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
pub struct HarnessStream {
    receiver: mpsc::Receiver<EngineEvent>,
    task: Option<JoinHandle<Result<TurnOutcome>>>,
    cancellation: CancellationToken,
}

impl HarnessStream {
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

impl Stream for HarnessStream {
    type Item = Result<EngineEvent>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Option<Self::Item>> {
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(event)) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for HarnessStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Fluent builder for an engine.
///
/// Three ports are required — a [`Store`], a [`BlobStore`] and a [`Model`] — and
/// [`build`](HarnessBuilder::build) fails with [`Error::InvalidInput`] naming the
/// one that is missing. Everything else has a default: an empty tool registry,
/// no policies, no layers, no observers and [`HarnessConfig::default`].
///
/// The ports are built once, at the edge of the program, from configuration; the
/// rest of the application sees [`Harness`] and never a vendor. A knob left unset
/// keeps its default behaviour, so a builder lists only what the host decided.
///
/// # Example
///
/// ```ignore
/// use std::sync::Arc;
///
/// use norte_core::{Capabilities, Message, Modality, ToolRegistry};
/// use norte_harness::{HarnessBuilder, InMemoryBlobStore, InMemoryStore};
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
/// let engine = HarnessBuilder::new()
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
pub struct HarnessBuilder {
    store: Option<Arc<dyn Store>>,
    blobs: Option<Arc<dyn BlobStore>>,
    model: Option<Arc<dyn Model>>,
    tools: ToolRegistry,
    policies: PolicySet,
    layers: LayerStack,
    interceptors: Vec<Arc<dyn TurnInterceptor>>,
    observers: Vec<Arc<dyn Observer>>,
    transforms: Vec<Arc<dyn StreamTransform>>,
    config: HarnessConfig,
    media: Option<Arc<dyn MediaPreprocessor>>,
    media_adapter: Option<Arc<dyn MediaAdapter>>,
}

/// Fluent builder for a single turn (FR-ENG-7, FR-ENG-8).
///
/// It borrows the engine; [`start`](TurnBuilder::start) hands back an
/// [`HarnessStream`] and the borrow ends there. Every knob is optional and
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
    engine: &'a Harness,
    session: SessionId,
    input: Option<Message>,
    options: TurnOptions,
}

impl<'a> TurnBuilder<'a> {
    fn new(engine: &'a Harness, session: SessionId) -> Self {
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
    /// [`Feature::Logprobs`](norte_core::Feature::Logprobs); the engine rejects the
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
    pub fn start(self) -> HarnessStream {
        self.engine
            .spawn_with(self.session, self.input, self.options)
    }
}

impl HarnessBuilder {
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
    /// Required. [`InMemoryBlobStore`](norte_harness::InMemoryBlobStore) is the
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
    /// [`ObserverSet::dropped`](norte_core::ObserverSet::dropped) counts the drops.
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
    pub fn config(mut self, config: HarnessConfig) -> Self {
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
    /// [`HarnessConfig::limits`](HarnessConfig::limits).
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] naming the port that is missing.
    pub fn build(self) -> Result<Harness> {
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
        Ok(Harness {
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

pub use crate::memory::InMemoryStore;
pub use norte_core::blob::InMemoryBlobStore;
