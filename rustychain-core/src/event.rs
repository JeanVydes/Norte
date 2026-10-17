use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::ReasoningRequest;
use crate::domain::part::Part;
use crate::domain::session::Phase;
use crate::id::{ApprovalId, CallId, Digest, SessionId, ToolName};
use crate::model::GenerationParams;

/// A streamed delta as observers see it. Continuity material is reduced to its
/// length: token payloads never enter the event channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "delta")]
#[non_exhaustive]
pub enum StreamDelta {
    Text {
        index: u32,
        text: String,
    },
    Reasoning {
        index: u32,
        text: String,
    },
    Refusal {
        index: u32,
        text: String,
    },
    ToolArguments {
        index: u32,
        call_id: CallId,
        arguments: String,
    },
    Media {
        index: u32,
        bytes: usize,
        last: bool,
    },
    Continuity {
        index: u32,
        len: usize,
    },
}

/// Typed notification channel. Durable events are written to the store before
/// they are emitted; observers may lose non-critical events under back-pressure
/// but never durability ordering.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EngineEvent {
    SessionCreated {
        session: SessionId,
        phase: Phase,
    },
    PhaseChanged {
        session: SessionId,
        from: Phase,
        to: Phase,
    },
    StreamDelta {
        session: SessionId,
        turn: u64,
        delta: StreamDelta,
    },
    PartCompleted {
        session: SessionId,
        turn: u64,
        part: Part,
    },
    ToolCallStarted {
        session: SessionId,
        turn: u64,
        call_id: CallId,
        tool: ToolName,
    },
    ToolCallFinished {
        session: SessionId,
        turn: u64,
        call_id: CallId,
        tool: ToolName,
        is_error: bool,
        duration_ms: u64,
    },
    ApprovalRequested {
        session: SessionId,
        approval: ApprovalId,
        call_id: CallId,
        tool: ToolName,
    },
    ApprovalGranted {
        session: SessionId,
        approval: ApprovalId,
    },
    /// A host seeded durable history onto the session head without generating
    /// (FR-ENG-6).
    HistoryAppended {
        session: SessionId,
        messages: usize,
    },
    CommitSucceeded {
        session: SessionId,
        version: u64,
    },
    CommitConflict {
        session: SessionId,
        expected: Option<u64>,
        actual: u64,
    },
    LayerShortCircuited {
        session: SessionId,
        layer: String,
        reason: crate::layer::ShortCircuitReason,
    },
    /// The engine reconstructed a request: the digest over the exact wire, the
    /// deterministic cache boundary positions and the effective request
    /// configuration (FR-ENG-9, FR-EVT-4).
    RequestReconstructed {
        session: SessionId,
        turn: u64,
        digest: Digest,
        cache_boundaries: Vec<u32>,
        params: GenerationParams,
        reasoning: ReasoningRequest,
        tools: Vec<ToolName>,
    },
    /// A host action invalidated the cached prefix; the reason names the knob
    /// that caused it (FR-EVT-4, FR-RECON-9).
    PrefixInvalidated {
        session: SessionId,
        reason: String,
    },
    Error {
        session: Option<SessionId>,
        message: String,
    },
    Extension {
        namespace: String,
        data: Value,
    },
}

impl EngineEvent {
    #[must_use]
    pub fn session(&self) -> Option<SessionId> {
        match self {
            EngineEvent::SessionCreated { session, .. }
            | EngineEvent::PhaseChanged { session, .. }
            | EngineEvent::StreamDelta { session, .. }
            | EngineEvent::PartCompleted { session, .. }
            | EngineEvent::ToolCallStarted { session, .. }
            | EngineEvent::ToolCallFinished { session, .. }
            | EngineEvent::ApprovalRequested { session, .. }
            | EngineEvent::ApprovalGranted { session, .. }
            | EngineEvent::HistoryAppended { session, .. }
            | EngineEvent::CommitSucceeded { session, .. }
            | EngineEvent::CommitConflict { session, .. }
            | EngineEvent::LayerShortCircuited { session, .. }
            | EngineEvent::RequestReconstructed { session, .. }
            | EngineEvent::PrefixInvalidated { session, .. } => Some(*session),
            EngineEvent::Error { session, .. } => *session,
            EngineEvent::Extension { .. } => None,
        }
    }
}

/// Minimal sink layers use to publish side-channel events without depending on
/// the engine.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: EngineEvent);
}

impl EventSink for tokio::sync::mpsc::Sender<EngineEvent> {
    fn emit(&self, event: EngineEvent) {
        if self.try_send(event).is_err() {
            tracing::debug!("engine event dropped: sink full or closed");
        }
    }
}

impl EventSink for std::sync::Arc<dyn Fn(EngineEvent) + Send + Sync> {
    fn emit(&self, event: EngineEvent) {
        self(event);
    }
}
