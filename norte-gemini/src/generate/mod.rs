mod request;
mod stream;

#[cfg(feature = "fuzz")]
#[doc(hidden)]
pub use stream::FuzzGenerate;

use norte_core::{
    Capabilities, ContinuityAdapter, Model, ModelId, ModelRef, ModelRequest, ModelStream, Result,
    WireFamily,
};
use norte_http::{HttpClient, VendorProfile};
use serde_json::Value;

use crate::WIRE_GENERATE;
use crate::continuity::GeminiContinuity;

/// The `Gemini` `generateContent` API adapter.
///
/// Streaming is the only primitive: the engine always opens an `SSE` response
/// and reassembles it, so a blocking call is just a collected stream.
#[derive(Clone)]
pub struct GeminiGenerate {
    reference: ModelRef,
    capabilities: Capabilities,
    profile: VendorProfile,
    client: HttpClient,
    continuity: GeminiContinuity,
}

impl GeminiGenerate {
    /// # Errors
    ///
    /// Returns an error when the wire family name or the default `HTTP` client
    /// cannot be built.
    pub fn new(
        reference: ModelRef,
        profile: VendorProfile,
        capabilities: Capabilities,
    ) -> Result<Self> {
        let family = WireFamily::new(WIRE_GENERATE)?;
        Ok(Self {
            continuity: GeminiContinuity::new(family, reference.clone()),
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
        request::build(&self.reference, &self.continuity, request)
    }

    /// Counts the tokens a request would consume, using the vendor's
    /// `countTokens` endpoint so context limits can be checked before the call.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be expressed on this wire or the
    /// endpoint fails.
    pub async fn count_tokens(&self, request: &ModelRequest) -> Result<u64> {
        let body = request::build_count(&self.reference, &self.continuity, request)?;
        let path = count_path(&self.reference.model);
        let response = self
            .client
            .send_post_json(&self.profile, &path, &body)
            .await?;
        response
            .get("totalTokens")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                norte_core::Error::Codec(
                    "gemini wire: countTokens response has no totalTokens".to_owned(),
                )
            })
    }

    /// Creates an explicit cached content from `request` and returns its
    /// resource name (for example `cachedContents/abc`).
    ///
    /// Gemini also caches prompt prefixes implicitly, which is what the adapter
    /// relies on by default. This is the opt-in explicit API; the returned name
    /// is sent on a turn as the `google.gemini:cachedContent` vendor extension
    /// (FR-ENG-7), so the cached prefix is served without being re-sent.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be expressed on this wire or the
    /// endpoint fails.
    pub async fn create_cached_content(&self, request: &ModelRequest, ttl: &str) -> Result<String> {
        let body = request::build_cache(&self.reference, &self.continuity, request, ttl)?;
        let response = self
            .client
            .send_post_json(&self.profile, "cachedContents", &body)
            .await?;
        response
            .get("name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                norte_core::Error::Codec(
                    "gemini wire: cachedContents response has no name".to_owned(),
                )
            })
    }
}

/// Builds the streaming `URL` path for a model, kept next to the adapter that
/// owns the wire shape.
fn stream_path(model: &ModelId) -> String {
    let name = model.as_str();
    let name = name.strip_prefix("models/").unwrap_or(name);
    format!("models/{name}:streamGenerateContent?alt=sse")
}

/// Builds the `countTokens` path for a model.
fn count_path(model: &ModelId) -> String {
    let name = model.as_str();
    let name = name.strip_prefix("models/").unwrap_or(name);
    format!("models/{name}:countTokens")
}

#[async_trait::async_trait]
impl Model for GeminiGenerate {
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
        let path = stream_path(&self.reference.model);
        let sse = self.client.send_sse(&self.profile, &path, &body).await?;
        Ok(stream::map(sse, self.continuity.clone()))
    }
}
