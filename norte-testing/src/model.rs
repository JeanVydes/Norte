use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures_util::StreamExt;

use norte_core::{
    Capabilities, ContinuityAdapter, ContinuityFragment, ContinuityScope, ContinuitySemantics,
    ContinuityToken, FinishReason, Modality, Model, ModelRef, ModelRequest, ModelStream, Part,
    Reasoning, ReasoningEffort, ReplayDecision, Result, StreamEvent, StreamingGranularity,
    ToolKind, WireFamily,
};
use serde_json::Value;

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A [`ContinuityAdapter`] that just concatenates opaque bytes and replays
/// tokens with the default scope table. Enough to exercise the engine and the
/// accumulator without a vendor.
#[derive(Clone, Copy, Debug, Default)]
pub struct OpaqueContinuity;

#[async_trait::async_trait]
impl ContinuityAdapter for OpaqueContinuity {
    fn build(&self, wire: &Value) -> Result<ContinuityToken> {
        Ok(ContinuityToken::new(
            WireFamily::new("test.opaque")?,
            ContinuityScope::Any,
            1,
            serde_json::to_vec(wire)?,
        ))
    }

    fn merge(
        &self,
        base: Option<ContinuityToken>,
        fragment: ContinuityFragment,
    ) -> Result<ContinuityToken> {
        let mut payload = base
            .as_ref()
            .map(|token| token.payload().to_vec())
            .unwrap_or_default();
        match fragment.semantics {
            ContinuitySemantics::Append => payload.extend_from_slice(&fragment.bytes),
            _ => payload.clone_from(&fragment.bytes),
        }
        Ok(ContinuityToken::new(
            fragment.family,
            fragment.scope,
            fragment.schema_version,
            payload,
        ))
    }

    fn replay(
        &self,
        target: &ModelRef,
        _history: &[norte_core::Message],
        token: &ContinuityToken,
    ) -> ReplayDecision {
        ReplayDecision::default_for(target, token)
    }
}

/// A continuity adapter that replays only for its own exact model and, when a
/// tool result is still pending, refuses a cross-model replay with
/// [`ReplayDecision::Block`] instead of dropping the token (FR-CONT-2,
/// FR-CONT-4). Vendors whose reasoning/thought signatures must survive a tool
/// loop behave this way.
#[derive(Clone, Debug)]
pub struct StrictContinuity {
    model: ModelRef,
}

impl StrictContinuity {
    #[must_use]
    pub fn new(model: ModelRef) -> Self {
        Self { model }
    }

    fn family() -> Result<WireFamily> {
        WireFamily::new("test.strict")
    }
}

#[async_trait::async_trait]
impl ContinuityAdapter for StrictContinuity {
    fn build(&self, wire: &Value) -> Result<ContinuityToken> {
        Ok(ContinuityToken::new(
            Self::family()?,
            ContinuityScope::Exact(self.model.clone()),
            1,
            serde_json::to_vec(wire)?,
        ))
    }

    fn merge(
        &self,
        base: Option<ContinuityToken>,
        fragment: ContinuityFragment,
    ) -> Result<ContinuityToken> {
        let mut payload = base
            .as_ref()
            .map(|token| token.payload().to_vec())
            .unwrap_or_default();
        match fragment.semantics {
            ContinuitySemantics::Append => payload.extend_from_slice(&fragment.bytes),
            _ => payload.clone_from(&fragment.bytes),
        }
        Ok(ContinuityToken::new(
            fragment.family,
            fragment.scope,
            fragment.schema_version,
            payload,
        ))
    }

    fn replay(
        &self,
        target: &ModelRef,
        history: &[norte_core::Message],
        token: &ContinuityToken,
    ) -> ReplayDecision {
        match ReplayDecision::default_for(target, token) {
            ReplayDecision::Replay => ReplayDecision::Replay,
            _ if history.iter().any(|message| {
                message
                    .parts
                    .iter()
                    .any(|part| matches!(part, Part::ToolResult(_)))
            }) =>
            {
                ReplayDecision::Block(
                    "strict continuity: cannot switch models mid tool loop".to_owned(),
                )
            }
            _ => ReplayDecision::Drop,
        }
    }
}

/// A deterministic [`Model`]: each call pops one scripted turn and records the
/// request it received.
pub struct ScriptedModel {
    reference: ModelRef,
    capabilities: Capabilities,
    turns: Mutex<VecDeque<(Vec<StreamEvent>, Duration)>>,
    requests: Mutex<Vec<ModelRequest>>,
    continuity: Arc<dyn ContinuityAdapter>,
}

impl ScriptedModel {
    #[must_use]
    pub fn new(reference: ModelRef) -> Self {
        Self {
            reference,
            capabilities: text_capabilities(),
            turns: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            continuity: Arc::new(OpaqueContinuity),
        }
    }

    /// Replaces the continuity adapter, so scenarios that depend on vendor token
    /// semantics can drive the engine (e.g. [`StrictContinuity`]).
    #[must_use]
    pub fn with_continuity(mut self, continuity: Arc<dyn ContinuityAdapter>) -> Self {
        self.continuity = continuity;
        self
    }

    #[must_use]
    pub fn with_capabilities(mut self, capabilities: Capabilities) -> Self {
        self.capabilities = capabilities;
        self
    }

    /// Queues one raw turn.
    #[must_use]
    pub fn turn(self, events: Vec<StreamEvent>) -> Self {
        self.turn_delayed(events, Duration::ZERO)
    }

    /// Queues one raw turn, pausing `delay` before every event so cancellation
    /// and back-pressure are observable.
    #[must_use]
    pub fn turn_delayed(self, events: Vec<StreamEvent>, delay: Duration) -> Self {
        guard(&self.turns).push_back((events, delay));
        self
    }

    /// Queues a plain text turn that ends with [`FinishReason::Stop`].
    #[must_use]
    pub fn turn_text(self, text: impl Into<String>) -> Self {
        let text = text.into();
        self.turn(vec![
            StreamEvent::TextDelta { index: 0, text },
            StreamEvent::Finish(FinishReason::Stop),
        ])
    }

    /// Queues a turn that emits one tool call and finishes with
    /// [`FinishReason::ToolCalls`].
    #[must_use]
    pub fn turn_tool_call(
        self,
        call_id: norte_core::CallId,
        name: impl Into<String>,
        arguments: &Value,
    ) -> Self {
        let name = name.into();
        self.turn(vec![
            StreamEvent::ToolCallStart {
                index: 0,
                call_id,
                name,
            },
            StreamEvent::ToolCallDelta {
                index: 0,
                call_id,
                name: None,
                arguments: arguments.to_string(),
            },
            StreamEvent::Finish(FinishReason::ToolCalls),
        ])
    }

    /// Queues a stream of text deltas split from `text`, so streaming is
    /// observable across several chunks.
    #[must_use]
    pub fn turn_deltas(self, parts: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut events = Vec::new();
        for (index, part) in parts.into_iter().enumerate() {
            if index == 0 {
                events.push(StreamEvent::TextStart { index: 0 });
            }
            events.push(StreamEvent::TextDelta {
                index: 0,
                text: part.into(),
            });
        }
        events.push(StreamEvent::Finish(FinishReason::Stop));
        self.turn(events)
    }

    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        guard(&self.requests).clone()
    }

    #[must_use]
    pub fn request_count(&self) -> usize {
        guard(&self.requests).len()
    }

    #[must_use]
    pub fn remaining_turns(&self) -> usize {
        guard(&self.turns).len()
    }
}

#[async_trait::async_trait]
impl Model for ScriptedModel {
    fn reference(&self) -> &ModelRef {
        &self.reference
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.continuity.as_ref()
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        guard(&self.requests).push(request);
        let (events, delay) = guard(&self.turns)
            .pop_front()
            .unwrap_or_else(|| (Vec::new(), Duration::ZERO));
        let stream = futures_util::stream::iter(events).then(move |event| async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(event)
        });
        Ok(Box::pin(stream))
    }
}

impl std::fmt::Debug for ScriptedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptedModel")
            .field("reference", &self.reference)
            .field("remaining_turns", &self.remaining_turns())
            .finish_non_exhaustive()
    }
}

/// Text-only, function-tool, delta-streaming capabilities. The sensible
/// baseline for a scripted model.
#[must_use]
pub fn text_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .reasoning(Reasoning {
            supported: true,
            can_disable: true,
            efforts: [
                ReasoningEffort::Minimal,
                ReasoningEffort::Low,
                ReasoningEffort::Medium,
                ReasoningEffort::High,
            ]
            .into_iter()
            .collect(),
            supports_budget: true,
            max_budget: Some(32_000),
        })
        .build()
}

/// A model that always fails its stream call, for error-path tests.
pub struct FailingModel {
    reference: ModelRef,
    capabilities: Capabilities,
    continuity: OpaqueContinuity,
    message: String,
}

impl FailingModel {
    #[must_use]
    pub fn new(reference: ModelRef, message: impl Into<String>) -> Self {
        Self {
            reference,
            capabilities: text_capabilities(),
            continuity: OpaqueContinuity,
            message: message.into(),
        }
    }
}

#[async_trait::async_trait]
impl Model for FailingModel {
    fn reference(&self) -> &ModelRef {
        &self.reference
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        &self.continuity
    }

    async fn stream(&self, _request: ModelRequest) -> Result<ModelStream> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        Err(norte_core::Error::Other(self.message.clone()))
    }
}
