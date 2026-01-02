use crate::execution::events::Event;
use crate::execution::{
    context::Context, environment::ExecutionEnvironment, events::InterceptionCommand,
};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Listener trait for handling events
#[async_trait]
pub trait EventListener<S, EV, T>: Send + Sync
where
    S: Into<String> + Clone + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static,
    T: Send + Sync + 'static,
{
    /// Process an event. Interceptors must return Some(response), observers return None
    async fn on_event(
        &self,
        event: &Event,
        context: &Context<S, EV, T>,
    ) -> Option<InterceptionCommand>;
}

pub type ArcEventListener<S, EV, T> = Arc<dyn EventListener<S, EV, T>>;
pub type InterceptorEntry<S, EV, T> = (i32, ArcEventListener<S, EV, T>);

/// Registry for managing event listeners
pub struct ListenerRegistry<S, EV, T> {
    observers: Arc<Mutex<Vec<ArcEventListener<S, EV, T>>>>,
    interceptors: Arc<Mutex<Vec<InterceptorEntry<S, EV, T>>>>,
}

impl<S, EV, T> ListenerRegistry<S, EV, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static,
    T: Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            observers: Arc::new(Mutex::new(Vec::new())),
            interceptors: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Register an observer (non-blocking)
    pub async fn add_observer(&self, listener: Arc<dyn EventListener<S, EV, T>>) {
        self.observers.lock().await.push(listener);
    }

    /// Register an interceptor with priority (higher = runs first)
    pub async fn add_interceptor(&self, listener: Arc<dyn EventListener<S, EV, T>>, priority: i32) {
        let mut interceptors = self.interceptors.lock().await;
        interceptors.push((priority, listener));
        interceptors.sort_by(|a, b| b.0.cmp(&a.0));
    }

    /// Emit an observable event to all observers
    pub async fn emit_observable<'a>(&self, event: &Event<'a>, context: &Context<S, EV, T>) {
        let observers = self.observers.lock().await;
        for observer in observers.iter() {
            let _ = observer.on_event(event, context).await;
        }
    }

    /// Emit an interceptable event and get the first interception response
    pub async fn emit_interceptable<'a>(
        &self,
        event: &Event<'a>,
        context: &Context<S, EV, T>,
    ) -> Option<InterceptionCommand> {
        // First notify observers
        self.emit_observable(event, context).await;

        // Then check interceptors in priority order
        let interceptors = self.interceptors.lock().await;
        for (_, interceptor) in interceptors.iter() {
            if let Some(response) = interceptor.on_event(event, context).await {
                return Some(response);
            }
        }

        None
    }
}

impl<S, EV, T> Default for ListenerRegistry<S, EV, T>
where
    S: Into<String> + Clone + Send + Sync + 'static,
    EV: ExecutionEnvironment + 'static,
    T: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}
