//! Host hooks the engine consults at the safe points of a running turn.

use std::sync::Arc;

use crate::domain::session::Phase;
use crate::event::EventSink;
use norte_core::domain::message::Message;
use norte_core::error::Result;
use norte_core::id::{NodeId, SessionId};

/// What an interceptor decided about the turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Interception {
    /// The turn goes on as it would.
    #[default]
    Continue,
    /// These messages join the session before the next request.
    Inject(Vec<Message>),
    /// The turn ends here.
    Stop {
        /// Why the turn ends. The host reports it under its own namespace.
        reason: String,
    },
}

/// What an interceptor may rely on while a turn runs.
pub struct InterceptionContext<'a> {
    /// The session the turn belongs to.
    pub session: SessionId,
    /// The turn number.
    pub turn: u64,
    /// The head the next request is built from.
    pub head: Option<NodeId>,
    /// The phase the turn is in.
    pub phase: Phase,
    /// The history the next request carries, oldest first.
    ///
    /// This is what the model will see, so an interceptor can decide on the
    /// conversation itself: the call the model just asked for, the tool that
    /// answered it, the instruction the human gave.
    pub messages: &'a [Message],
    /// The sink an interceptor uses to emit engine events.
    pub events: &'a dyn EventSink,
}

/// A host hook the engine consults before every model request of a turn.
///
/// An interceptor may return messages to append to the session's history. The
/// engine appends them itself, so they are durable, part of the request
/// digest, visible to the policies, and free of a version race: while a turn
/// runs, the engine is the only writer.
///
/// This is the seam a steering message takes when it must reach the request
/// that is already in flight. A [`Layer`](norte_core::layer::Layer) cannot do it: a
/// layer sees the request after it was built, so what it adds is neither
/// durable nor visible to the policies.
#[async_trait::async_trait]
pub trait TurnInterceptor: Send + Sync {
    /// The interceptor's name, as it appears in the session metadata.
    fn name(&self) -> &str;

    /// Decides what the turn does next.
    ///
    /// The engine consults the interceptor before the turn's input and before
    /// every model request, so a message the interceptor returns reaches the
    /// request that is about to be built.
    ///
    /// # Errors
    ///
    /// Returns an error when the host cannot decide. The turn then fails with
    /// it and the session stays resumable.
    async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception>;
}

/// The interceptors an engine consults, in registration order.
#[derive(Clone, Default)]
pub struct InterceptorSet {
    interceptors: Vec<Arc<dyn TurnInterceptor>>,
}

impl InterceptorSet {
    /// Creates a set from the given interceptors.
    #[must_use]
    pub fn new(interceptors: Vec<Arc<dyn TurnInterceptor>>) -> Self {
        Self { interceptors }
    }

    /// The interceptors, in registration order.
    #[must_use]
    pub fn interceptors(&self) -> &[Arc<dyn TurnInterceptor>] {
        &self.interceptors
    }

    /// The names of the interceptors, for the session metadata.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.interceptors
            .iter()
            .map(|interceptor| interceptor.name())
            .collect()
    }

    /// Whether the set holds no interceptor.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.interceptors.is_empty()
    }
}
