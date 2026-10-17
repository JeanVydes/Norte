mod request;
mod stream;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub use stream::FuzzMessages;

use rustychain_core::{
    Capabilities, ContinuityAdapter, Model, ModelRef, ModelRequest, ModelStream, Result, WireFamily,
};
use rustychain_http::{HttpClient, VendorProfile};
use serde_json::Value;

use crate::WIRE_MESSAGES;
use crate::continuity::AnthropicContinuity;

/// Path appended to the vendor base `URL`.
const PATH: &str = "messages";

/// Path appended to the vendor base `URL` for token counting.
const COUNT_TOKENS_PATH: &str = "messages/count_tokens";

/// The `Anthropic` Messages API adapter.
///
/// Streaming is the only primitive: the engine always opens an `SSE` response
/// and reassembles it, so a blocking call is just a collected stream.
#[derive(Clone)]
pub struct AnthropicMessages {
    reference: ModelRef,
    capabilities: Capabilities,
    profile: VendorProfile,
    client: HttpClient,
    continuity: AnthropicContinuity,
}

impl AnthropicMessages {
    /// # Errors
    ///
    /// Returns an error when the wire family name or the default `HTTP` client
    /// cannot be built.
    pub fn new(
        reference: ModelRef,
        profile: VendorProfile,
        capabilities: Capabilities,
    ) -> Result<Self> {
        let family = WireFamily::new(WIRE_MESSAGES)?;
        Ok(Self {
            continuity: AnthropicContinuity::new(family, reference.clone()),
            reference,
            capabilities,
            profile,
            client: HttpClient::new()?,
        })
    }

    #[must_use]
    pub fn with_client(mut self, client: HttpClient) -> Self {
        self.client = client;
        self
    }

    #[must_use]
    pub fn profile(&self) -> &VendorProfile {
        &self.profile
    }

    /// The exact JSON body this adapter would `POST` for `request`.
    ///
    /// It is a pure function of the request, so a host can audit the wire — and
    /// therefore prompt-cache behaviour — without sending anything (FR-RECON-12).
    ///
    /// # Errors
    ///
    /// Returns a codec error when a part cannot be expressed on this wire.
    pub fn request_body(&self, request: &ModelRequest) -> Result<Value> {
        request::build(&self.reference, &self.continuity, request)
    }

    /// Counts the tokens a request would consume, using the vendor's
    /// `messages/count_tokens` endpoint so context limits can be checked before
    /// the call.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be expressed on this wire or the
    /// endpoint fails.
    pub async fn count_tokens(&self, request: &ModelRequest) -> Result<u64> {
        let body = request::build_count(&self.reference, &self.continuity, request)?;
        let response = self
            .client
            .send_post_json(&self.profile, COUNT_TOKENS_PATH, &body)
            .await?;
        response
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                rustychain_core::Error::Codec(
                    "anthropic wire: count_tokens response has no input_tokens".to_owned(),
                )
            })
    }
}

#[async_trait::async_trait]
impl Model for AnthropicMessages {
    fn reference(&self) -> &ModelRef {
        &self.reference
    }

    fn capabilities(&self) -> &Capabilities {
        &self.capabilities
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        &self.continuity
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream> {
        let body = request::build(&self.reference, &self.continuity, &request)?;
        let sse = self.client.send_sse(&self.profile, PATH, &body).await?;
        Ok(stream::map(sse, self.continuity.clone()))
    }
}
