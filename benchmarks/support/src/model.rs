//! A model wrapper that records the exact wire body of every request.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use norte_core::{
    Capabilities, ContinuityAdapter, Digest, Model, ModelRef, ModelRequest, ModelStream,
};
use norte_openai::OpenAiChat;
use serde::Serialize;
use serde_json::Value;

/// The requests a recording model has seen, in call order.
pub type Captured = Arc<Mutex<Vec<RequestRecord>>>;

/// One recorded request: what was sent, when, and its digest.
#[derive(Debug, Clone, Serialize)]
pub struct RequestRecord {
    /// One-based position in the call order.
    pub index: usize,
    /// When the request was about to be sent.
    pub at: DateTime<Utc>,
    /// The messages the request carried.
    pub messages: usize,
    /// The size of the projection, in bytes.
    pub bytes: usize,
    /// The digest of the projection, for cache attribution.
    pub digest: String,
    /// The exact body the adapter would `POST`.
    pub body: Value,
}

/// An `OpenAI`-chat model that records every request body and then makes the
/// call anyway, so a benchmark stays live: what is recorded is what was sent.
///
/// Before each request it waits `pause`, so a provider's asynchronous prefix
/// cache has time to publish the request before it. That is a property of the
/// measurement protocol, not of the model: without it, two turns fired back to
/// back can both miss a cache that would have served the second one a moment
/// later.
pub struct RecordingOpenAi {
    inner: OpenAiChat,
    captured: Captured,
    pause: Duration,
}

impl RecordingOpenAi {
    /// Wraps an adapter, the record it appends to, and the pause before each
    /// request.
    #[must_use]
    pub fn new(inner: OpenAiChat, captured: Captured, pause: Duration) -> Self {
        Self {
            inner,
            captured,
            pause,
        }
    }

    /// The requests recorded so far.
    #[must_use]
    pub fn captured(&self) -> Captured {
        Arc::clone(&self.captured)
    }
}

#[async_trait]
impl Model for RecordingOpenAi {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, request: ModelRequest) -> norte_core::Result<ModelStream> {
        if !self.pause.is_zero() {
            tokio::time::sleep(self.pause).await;
        }
        let body = self.inner.request_body(&request)?;
        {
            let mut captured = self
                .captured
                .lock()
                .expect("the probe lock is never held across an await");
            let index = captured.len() + 1;
            captured.push(RequestRecord {
                index,
                at: Utc::now(),
                messages: body
                    .get("messages")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
                bytes: body.to_string().len(),
                digest: Digest::of_json(&body).to_string(),
                body,
            });
        }
        self.inner.stream(request).await
    }
}

/// A fresh, empty record.
#[must_use]
pub fn captured() -> Captured {
    Arc::new(Mutex::new(Vec::new()))
}
