mod request;
mod stream;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub use stream::FuzzChat;

use rustychain_core::{
    Capabilities, ContinuityAdapter, Model, ModelRef, ModelRequest, ModelStream, Result, WireFamily,
};
use rustychain_http::{HttpClient, VendorProfile};
use serde_json::Value;

use crate::WIRE_CHAT;
use crate::continuity::OpenAiContinuity;

/// Path appended to the vendor base `URL`.
const PATH: &str = "chat/completions";

/// The vendor key under which a streamed audio id is preserved in a media
/// part's metadata, so an assistant turn can be replayed by reference.
pub(crate) const AUDIO_ID_KEY: &str = "openai.audio_id";

/// The `OpenAI` `Chat Completions` adapter.
///
/// It exists for the many wire-compatible vendors that never adopted the
/// Responses API. Reasoning is output-only here, so reasoning parts are not
/// replayed; tool-call ids survive in continuity.
#[derive(Clone)]
pub struct OpenAiChat {
    reference: ModelRef,
    capabilities: Capabilities,
    profile: VendorProfile,
    client: HttpClient,
    continuity: OpenAiContinuity,
}

impl OpenAiChat {
    /// # Errors
    ///
    /// Returns an error when the wire family name or the default `HTTP` client
    /// cannot be built.
    pub fn new(
        reference: ModelRef,
        profile: VendorProfile,
        capabilities: Capabilities,
    ) -> Result<Self> {
        let family = WireFamily::new(WIRE_CHAT)?;
        Ok(Self {
            continuity: OpenAiContinuity::new(family, reference.clone()),
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
        request::build(
            &self.reference,
            &self.continuity,
            self.profile.quirks(),
            request,
        )
    }
}

#[async_trait::async_trait]
impl Model for OpenAiChat {
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
        let body = request::build(
            &self.reference,
            &self.continuity,
            self.profile.quirks(),
            &request,
        )?;
        let sse = self.client.send_sse(&self.profile, PATH, &body).await?;
        Ok(stream::map(sse, self.continuity.clone()))
    }
}
