use std::sync::{Arc, Mutex};

use norte_core::{EngineEvent, Observer};

/// Records every observed event in memory for assertions.
#[derive(Clone)]
pub struct RecordingObserver {
    name: String,
    events: Arc<Mutex<Vec<EngineEvent>>>,
}

impl RecordingObserver {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[must_use]
    pub fn events(&self) -> Vec<EngineEvent> {
        self.events
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

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
