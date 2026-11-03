use std::sync::Arc;

use crate::error::Result;
use crate::id::SessionId;
use crate::model::{ModelRequest, ModelStream};
use crate::stream::StreamEvent;

/// What a layer may rely on while wrapping a request.
///
/// A layer wraps *one model call*: it sees the session and turn it belongs to
/// and nothing else. It has no sink and no way to report: what it decides is the
/// [`ModelStream`] it returns, and a stream that never reaches the model is a
/// short circuit by construction.
pub struct LayerContext {
    /// The session the turn belongs to.
    pub session: SessionId,
    /// The turn number.
    pub turn: u64,
}

/// A typed reason a layer may short-circuit a request (FR-LAYER-2).
///
/// A rejecting layer can return this through [`crate::Error::ShortCircuited`].
/// The host owns audit logging; returning a synthetic stream does not publish
/// a reason automatically.
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
pub fn synthetic_stream(events: Vec<StreamEvent>) -> ModelStream {
    Box::pin(futures_util::stream::iter(
        events.into_iter().map(Ok::<_, crate::error::Error>),
    ))
}

/// Typed middleware around the request/stream lifecycle.
///
/// A layer may forward a request, reject it, or return a synthetic stream.
/// Persistence and audit logging belong to the host, not this contract.
#[async_trait::async_trait]
pub trait Layer: Send + Sync {
    /// The layer's name, as it appears in the audit trail.
    fn name(&self) -> &str;

    /// Wraps `request`, either forwarding to `next` or answering with a
    /// synthetic stream.
    ///
    /// # Errors
    ///
    /// Returns whatever the layer or the call below it fails with.
    async fn wrap(
        &self,
        context: &LayerContext,
        request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream>;
}

/// The inner call a layer forwards to.
#[async_trait::async_trait]
pub trait Continuation: Send + Sync {
    /// Runs the continuation with `request`.
    ///
    /// # Errors
    ///
    /// Returns whatever the next layer or the model call fails with.
    async fn call(&self, context: &LayerContext, request: ModelRequest) -> Result<ModelStream>;
}

/// A fixed stack, outermost first, built once at construction.
#[derive(Clone, Default)]
pub struct LayerStack {
    layers: Vec<Arc<dyn Layer>>,
}

impl LayerStack {
    /// A stack of `layers`, outermost first.
    #[must_use]
    pub fn new(layers: Vec<Arc<dyn Layer>>) -> Self {
        Self { layers }
    }

    /// The layers, outermost first.
    #[must_use]
    pub fn layers(&self) -> &[Arc<dyn Layer>] {
        &self.layers
    }

    /// Runs the stack around `terminal`, the adapter call.
    ///
    /// # Errors
    ///
    /// Propagates the first error any layer or the terminal call returns.
    pub async fn run(
        &self,
        context: &LayerContext,
        request: ModelRequest,
        terminal: &dyn Continuation,
    ) -> Result<ModelStream> {
        Next {
            layers: &self.layers,
            terminal,
        }
        .call(context, request)
        .await
    }
}

struct Next<'a> {
    layers: &'a [Arc<dyn Layer>],
    terminal: &'a dyn Continuation,
}

#[async_trait::async_trait]
impl Continuation for Next<'_> {
    async fn call(&self, context: &LayerContext, request: ModelRequest) -> Result<ModelStream> {
        match self.layers.split_first() {
            Some((layer, rest)) => {
                let next = Self {
                    layers: rest,
                    terminal: self.terminal,
                };
                layer.wrap(context, request, &next).await
            }
            None => self.terminal.call(context, request).await,
        }
    }
}

/// Adapter call as a continuation, so the stack's innermost step can reach it.
pub struct ModelContinuation<'a> {
    /// The model port the stack's innermost step calls.
    pub model: &'a dyn crate::model::Model,
}

#[async_trait::async_trait]
impl Continuation for ModelContinuation<'_> {
    async fn call(&self, _context: &LayerContext, request: ModelRequest) -> Result<ModelStream> {
        self.model.stream(request).await
    }
}

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::domain::message::FinishReason;

    struct Recording {
        name: &'static str,
        calls: Arc<std::sync::Mutex<Vec<&'static str>>>,
        short_circuit: bool,
    }

    #[async_trait::async_trait]
    impl Layer for Recording {
        fn name(&self) -> &str {
            self.name
        }

        async fn wrap(
            &self,
            context: &LayerContext,
            request: ModelRequest,
            next: &dyn Continuation,
        ) -> Result<ModelStream> {
            self.calls.lock().expect("calls").push(self.name);
            if self.short_circuit {
                Ok(synthetic_stream(vec![StreamEvent::Finish(
                    FinishReason::Stop,
                )]))
            } else {
                next.call(context, request).await
            }
        }
    }

    #[async_trait::async_trait]
    impl Continuation for Recording {
        async fn call(&self, _: &LayerContext, _: ModelRequest) -> Result<ModelStream> {
            self.calls.lock().expect("calls").push(self.name);
            Ok(synthetic_stream(vec![StreamEvent::Finish(
                FinishReason::Stop,
            )]))
        }
    }

    #[tokio::test]
    async fn layers_run_outermost_first_and_short_circuit_without_calling_the_rest() {
        for (names, short_circuit, expected) in [
            (vec![], false, vec!["model"]),
            (
                vec!["outer", "inner"],
                false,
                vec!["outer", "inner", "model"],
            ),
            (vec!["outer", "inner"], true, vec!["outer"]),
        ] {
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let layers = names
                .into_iter()
                .map(|name| {
                    Arc::new(Recording {
                        name,
                        calls: calls.clone(),
                        short_circuit,
                    }) as Arc<dyn Layer>
                })
                .collect();
            let stack = LayerStack::new(layers);
            let terminal = Recording {
                name: "model",
                calls: calls.clone(),
                short_circuit: false,
            };
            let context = LayerContext {
                session: SessionId::new(),
                turn: 1,
            };
            let model = crate::ModelRef::new(
                crate::ProviderId::new("test").expect("provider"),
                crate::ModelId::new("model").expect("model"),
            );
            let events: Vec<_> = stack
                .run(&context, ModelRequest::new(model, vec![]), &terminal)
                .await
                .expect("stream")
                .collect()
                .await;
            assert_eq!(*calls.lock().expect("calls"), expected);
            assert!(matches!(
                events.as_slice(),
                [Ok(StreamEvent::Finish(FinishReason::Stop))]
            ));
        }
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
