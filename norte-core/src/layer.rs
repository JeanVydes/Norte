use std::sync::Arc;

use crate::error::Result;
use crate::event::{EngineEvent, EventSink};
use crate::id::SessionId;
use crate::model::{ModelRequest, ModelStream};
use crate::stream::StreamEvent;

pub struct LayerContext<'a> {
    pub session: SessionId,
    pub turn: u64,
    pub events: &'a dyn EventSink,
}

impl LayerContext<'_> {
    /// Records that `layer` short-circuited the request (FR-LAYER-2).
    ///
    /// The typed reason is emitted as an audit event. The layer is still the one
    /// that returns its own synthetic [`ModelStream`].
    pub fn short_circuit(&self, layer: &str, reason: ShortCircuitReason) {
        self.events.emit(EngineEvent::LayerShortCircuited {
            session: self.session,
            layer: layer.to_owned(),
            reason,
        });
    }
}

/// A typed reason a layer may short-circuit a request (FR-LAYER-2).
///
/// The reason is recorded in the audit trail, so the same decision is always
/// attributable.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShortCircuitReason {
    /// The layer answered from a cache without calling the model.
    CacheHit,
    /// The layer rejected the request before it reached the model.
    Rejected {
        /// Human-readable detail.
        detail: String,
    },
    /// The layer answered locally (for example, a deterministic tool).
    Local {
        /// Human-readable detail.
        detail: String,
    },
    /// A host-supplied reason.
    Custom {
        /// The host's name for the reason.
        name: String,
        /// Human-readable detail.
        detail: String,
    },
}

/// Builds a synthetic stream from a fixed sequence of events, for layers that
/// short-circuit instead of calling the model (FR-LAYER-2).
#[must_use]
pub fn synthetic_stream(events: Vec<StreamEvent>) -> ModelStream {
    Box::pin(futures_util::stream::iter(
        events.into_iter().map(Ok::<_, crate::error::Error>),
    ))
}

/// Typed middleware around the request/stream lifecycle.
///
/// Layers are pure with respect to the store: durable effects go through the
/// engine's commit path. A layer may short-circuit with a synthetic stream;
/// the reason it does so is recorded in the audit trail.
#[async_trait::async_trait]
pub trait Layer: Send + Sync {
    fn name(&self) -> &str;

    async fn wrap(
        &self,
        context: &LayerContext<'_>,
        request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream>;
}

/// The inner call a layer forwards to.
#[async_trait::async_trait]
pub trait Continuation: Send + Sync {
    async fn call(&self, context: &LayerContext<'_>, request: ModelRequest) -> Result<ModelStream>;
}

/// A fixed stack, outermost first, built once at construction.
#[derive(Clone, Default)]
pub struct LayerStack {
    layers: Vec<Arc<dyn Layer>>,
}

impl LayerStack {
    #[must_use]
    pub fn new(layers: Vec<Arc<dyn Layer>>) -> Self {
        Self { layers }
    }

    #[must_use]
    pub fn layers(&self) -> &[Arc<dyn Layer>] {
        &self.layers
    }

    /// Runs the stack around `terminal`, the adapter call.
    ///
    /// # Errors
    ///
    /// Propagates the first error any layer or the terminal call returns.
    pub async fn run<'a>(
        &'a self,
        context: &LayerContext<'_>,
        request: ModelRequest,
        terminal: &'a dyn Continuation,
    ) -> Result<ModelStream> {
        let mut next: Box<dyn Continuation + 'a> = Box::new(Terminal { inner: terminal });
        for layer in self.layers.iter().rev() {
            next = Box::new(Step {
                layer: Arc::clone(layer),
                next,
            });
        }
        next.call(context, request).await
    }
}

struct Terminal<'a> {
    inner: &'a dyn Continuation,
}

#[async_trait::async_trait]
impl Continuation for Terminal<'_> {
    async fn call(&self, context: &LayerContext<'_>, request: ModelRequest) -> Result<ModelStream> {
        self.inner.call(context, request).await
    }
}

struct Step<'a> {
    layer: Arc<dyn Layer>,
    next: Box<dyn Continuation + 'a>,
}

#[async_trait::async_trait]
impl Continuation for Step<'_> {
    async fn call(&self, context: &LayerContext<'_>, request: ModelRequest) -> Result<ModelStream> {
        self.layer.wrap(context, request, self.next.as_ref()).await
    }
}

/// Adapter call as a continuation, so the stack's innermost step can reach it.
pub struct ModelContinuation<'a> {
    pub model: &'a dyn crate::model::Model,
}

#[async_trait::async_trait]
impl Continuation for ModelContinuation<'_> {
    async fn call(
        &self,
        _context: &LayerContext<'_>,
        request: ModelRequest,
    ) -> Result<ModelStream> {
        self.model.stream(request).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use futures_util::StreamExt;

    use super::*;
    use crate::domain::message::FinishReason;

    #[derive(Default)]
    struct Sink(Mutex<Vec<EngineEvent>>);

    impl EventSink for Sink {
        fn emit(&self, event: EngineEvent) {
            self.0.lock().expect("lock").push(event);
        }
    }

    #[test]
    fn a_short_circuit_emits_a_typed_reason() {
        let sink = Sink::default();
        let context = LayerContext {
            session: SessionId::new(),
            turn: 2,
            events: &sink,
        };
        context.short_circuit(
            "cache",
            ShortCircuitReason::Rejected {
                detail: "blocked".to_owned(),
            },
        );
        let events = sink.0.lock().expect("lock");
        assert!(matches!(
            events.as_slice(),
            [EngineEvent::LayerShortCircuited { layer, reason, .. }]
                if layer == "cache"
                    && *reason
                        == ShortCircuitReason::Rejected {
                            detail: "blocked".to_owned(),
                        }
        ));
    }

    #[tokio::test]
    async fn a_synthetic_stream_yields_its_events_in_order() {
        let mut stream = synthetic_stream(vec![
            StreamEvent::Finish(FinishReason::Stop),
            StreamEvent::Finish(FinishReason::Length),
        ]);
        let first = stream.next().await.expect("first").expect("ok");
        assert!(matches!(first, StreamEvent::Finish(FinishReason::Stop)));
        let second = stream.next().await.expect("second").expect("ok");
        assert!(matches!(second, StreamEvent::Finish(FinishReason::Length)));
        assert!(stream.next().await.is_none());
    }
}
