use std::sync::{Arc, Mutex, MutexGuard};

use rustychain_core::{
    Capabilities, ContinuityAdapter, Model, ModelRef, ModelRequest, ModelStream, Result, StreamEvent,
};

fn guard<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A [`Model`] that delegates identity, capabilities and continuity to an inner
/// model, records every reconstructed [`ModelRequest`] the engine hands it, and
/// answers with a fixed stream instead of touching the network.
///
/// It is the seam for auditing exactly what would be sent — the reconstructed
/// request — without a socket, so cache byte-identity can be asserted in the
/// hermetic test suite (FR-TEST-1, FR-RECON-12).
#[derive(Clone)]
pub struct ProbeModel {
    inner: Arc<dyn Model>,
    requests: Arc<Mutex<Vec<ModelRequest>>>,
}

impl ProbeModel {
    /// Wraps `inner`, delegating everything except [`Model::stream`].
    #[must_use]
    pub fn new(inner: Arc<dyn Model>) -> Self {
        Self {
            inner,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Every request the engine reconstructed, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<ModelRequest> {
        guard(&self.requests).clone()
    }

    /// The most recent reconstructed request.
    #[must_use]
    pub fn last_request(&self) -> Option<ModelRequest> {
        guard(&self.requests).last().cloned()
    }
}

#[async_trait::async_trait]
impl Model for ProbeModel {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        guard(&self.requests).push(request);
        let events = vec![
            StreamEvent::TextDelta {
                index: 0,
                text: "ok".to_owned(),
            },
            StreamEvent::Finish(rustychain_core::FinishReason::Stop),
        ];
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
}
