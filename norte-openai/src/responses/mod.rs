mod request;
mod stream;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub use stream::FuzzResponses;

use norte_core::{
    Capabilities, ContinuityAdapter, Model, ModelRef, ModelRequest, ModelStream, Result, WireFamily,
};
use norte_http::{HttpClient, VendorProfile};
use serde_json::Value;

use crate::WIRE_RESPONSES;
use crate::continuity::OpenAiContinuity;

/// Path appended to the vendor base `URL`.
const PATH: &str = "responses";

/// The `OpenAI` Responses API adapter.
///
/// Streaming is the only primitive: the engine always opens an `SSE` response
/// and reassembles it, so a blocking call is just a collected stream.
#[derive(Clone)]
pub struct OpenAiResponses {
    reference: ModelRef,
    capabilities: Capabilities,
    profile: VendorProfile,
    client: HttpClient,
    continuity: OpenAiContinuity,
}

impl OpenAiResponses {
    /// # Errors
    ///
    /// Returns an error when the wire family name or the default `HTTP` client
    /// cannot be built.
    pub fn new(
        reference: ModelRef,
        profile: VendorProfile,
        capabilities: Capabilities,
    ) -> Result<Self> {
        let family = WireFamily::new(WIRE_RESPONSES)?;
        Ok(Self {
            continuity: OpenAiContinuity::new(family, reference.clone()),
            reference,
            capabilities,
            profile,
            client: HttpClient::new()?,
        })
    }

    /// Reuses an existing HTTP client instead of building one.
    #[must_use]
    pub fn with_client(mut self, client: HttpClient) -> Self {
        self.client = client;
        self
    }

    /// The vendor profile this adapter sends to.
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
        request::build_with_quirks(
            &self.reference,
            &self.continuity,
            self.profile.quirks(),
            request,
        )
    }
}

#[async_trait::async_trait]
impl Model for OpenAiResponses {
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
        let body = request::build_with_quirks(
            &self.reference,
            &self.continuity,
            self.profile.quirks(),
            &request,
        )?;
        let sse = self.client.send_sse(&self.profile, PATH, &body).await?;
        Ok(stream::map(sse, self.continuity.clone()))
    }
}
