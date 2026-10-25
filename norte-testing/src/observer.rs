use std::sync::{Arc, Mutex};

use norte_harness::{EngineEvent, Observer};

/// Records every observed event in memory for assertions.
#[derive(Clone)]
pub struct RecordingObserver {
    name: String,
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl RecordingObserver {
    /// An observer named `name` that has seen nothing yet.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// The events seen so far, in order.
    #[must_use]
    pub fn events(&self) -> Vec<EngineEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// How many events have been seen.
    #[must_use]
    pub fn count(&self) -> usize {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl Observer for RecordingObserver {
    fn name(&self) -> &str {
        &self.name
    }

    fn observe(&self, event: &EngineEvent) {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(event.clone());
    }
}
