use std::collections::BTreeMap;
use std::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::{Capabilities, ReasoningRequest};
use crate::domain::continuity::{ContinuityFragment, ContinuityToken, ReplayDecision};
use crate::domain::message::Message;
use crate::error::Result;
use crate::id::{ModelRef, SessionId, ToolName};
use crate::stream::StreamEvent;
use crate::tool::ToolSpec;

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send + 'static>>;

/// Request extension key carrying the originating session id, so adapters can
/// derive a stable provider cache key without inventing their own identity.
pub const SESSION_EXTENSION: &str = "session";

/// Session-metadata key carrying the effective request options of the last turn
/// (FR-EXP-7). It is engine-managed; hosts must not write it.
pub const TURN_OPTIONS_KEY: &str = "rustychain.turn_options";

/// Sampling and output controls shared by every adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// JSON Schema the output must satisfy, when the model supports structured
    /// output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl GenerationParams {
    /// True when every field is unset, i.e. the parameters change nothing.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Overlays `self` on `base`: every set field wins, every unset field
    /// inherits from `base` (FR-ENG-7).
    ///
    /// An empty `stop_sequences` means "unset" and therefore inherits; a
    /// non-empty list replaces `base` entirely.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        Self {
            temperature: self.temperature.or(base.temperature),
            top_p: self.top_p.or(base.top_p),
            top_k: self.top_k.or(base.top_k),
            max_output_tokens: self.max_output_tokens.or(base.max_output_tokens),
            stop_sequences: if self.stop_sequences.is_empty() {
                base.stop_sequences.clone()
            } else {
                self.stop_sequences.clone()
            },
            seed: self.seed.or(base.seed),
            output_schema: self
                .output_schema
                .clone()
                .or_else(|| base.output_schema.clone()),
        }
    }
}

/// Per-turn request overrides (FR-ENG-7).
///
/// Layered least-to-most specific — engine defaults ← session defaults ← turn
/// overrides — and merged per field: a field set here replaces that field only;
/// an unset field inherits from the layer below. The effective options are
/// exactly what the adapter receives (FR-ENG-9).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnOptions {
    /// Generation-parameter overrides; unset fields inherit.
    #[serde(default, skip_serializing_if = "GenerationParams::is_default")]
    pub params: GenerationParams,
    /// Reasoning override; `None` inherits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningRequest>,
    /// Exposed tool subset by name; `None` exposes every registered tool
    /// (FR-ENG-8). Order is canonicalized at request time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolName>>,
}

impl TurnOptions {
    /// True when the options change nothing relative to the layer below.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.params.is_default() && self.reasoning.is_none() && self.tools.is_none()
    }

    /// Overlays `self` on `base`: set fields win, unset fields inherit.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        Self {
            params: self.params.over(&base.params),
            reasoning: self.reasoning.or(base.reasoning),
            tools: self.tools.clone().or_else(|| base.tools.clone()),
        }
    }
}

/// Bounds that keep a stream's memory predictable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLimits {
    /// Maximum bytes held for one unfinished part.
    pub max_part_bytes: usize,
    /// Maximum events buffered between the engine and a consumer.
    pub max_buffered_events: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_part_bytes: 8 * 1024 * 1024,
            max_buffered_events: 256,
        }
    }
}

/// Everything an adapter needs to produce one generation. Messages are the
/// whole conversation, in order; the last one is what the model must answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelRef,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub reasoning: ReasoningRequest,
    pub params: GenerationParams,
    pub limits: StreamLimits,
    /// Positions (exclusive end indices into `messages`) at which the provider
    /// may place a prompt-cache breakpoint. Deterministic: computed by
    /// reconstruction and unchanged when a new turn is appended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_boundaries: Vec<u32>,
    pub extensions: BTreeMap<String, Value>,
}

impl ModelRequest {
    #[must_use]
    pub fn new(model: ModelRef, messages: Vec<Message>) -> Self {
        Self {
            model,
            messages,
            tools: Vec::new(),
            reasoning: ReasoningRequest::default(),
            params: GenerationParams::default(),
            limits: StreamLimits::default(),
            cache_boundaries: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }
}

/// Adapter-owned interpretation of continuity tokens.
///
/// Core stores and moves tokens; only the adapter that produced them knows how
/// to build, merge and replay them.
pub trait ContinuityAdapter: Send + Sync {
    /// Builds a token from vendor wire state.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the wire value cannot be encoded.
    fn build(&self, wire: &Value) -> Result<ContinuityToken>;

    /// Merges a streamed fragment into the token under construction.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the fragment is malformed.
    fn merge(
        &self,
        base: Option<ContinuityToken>,
        fragment: ContinuityFragment,
    ) -> Result<ContinuityToken>;

    fn replay(
        &self,
        target: &ModelRef,
        history: &[Message],
        token: &ContinuityToken,
    ) -> ReplayDecision;
}

/// The single model port. Streaming is the primitive; a blocking result is the
/// collected stream.
#[async_trait::async_trait]
pub trait Model: Send + Sync {
    fn reference(&self) -> &ModelRef;

    fn capabilities(&self) -> &Capabilities;

    fn continuity(&self) -> &dyn ContinuityAdapter;

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
}

/// Bidirectional session contract. It reuses the store, approvals, tools and
/// policies, but owns its transport lifecycle.
#[async_trait::async_trait]
pub trait RealtimeModel: Send + Sync {
    fn reference(&self) -> &ModelRef;

    fn capabilities(&self) -> &Capabilities;

    async fn connect(&self, session: SessionId) -> Result<Box<dyn RealtimeSession>>;
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum RealtimeEvent {
    AudioInput {
        bytes: Vec<u8>,
    },
    AudioOutput {
        bytes: Vec<u8>,
    },
    TextDelta {
        text: String,
    },
    Transcript {
        role: crate::domain::role::Role,
        text: String,
    },
    ToolCall {
        call_id: crate::id::CallId,
        name: String,
        arguments: Value,
    },
    TurnEnded,
    Error {
        message: String,
    },
    Extension {
        namespace: String,
        data: Value,
    },
}

pub type RealtimeEventStream = Pin<Box<dyn Stream<Item = Result<RealtimeEvent>> + Send + 'static>>;

#[async_trait::async_trait]
pub trait RealtimeSession: Send + Sync {
    async fn send_audio(&self, bytes: Vec<u8>) -> Result<()>;

    async fn send_text(&self, text: String) -> Result<()>;

    async fn events(&mut self) -> Result<RealtimeEventStream>;

    async fn close(&mut self) -> Result<()>;
}
