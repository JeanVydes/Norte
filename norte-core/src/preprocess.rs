use crate::domain::media::{MediaKind, MimeType};
use crate::error::Result;

/// Inline media bytes plus their type: the unit a preprocessor transforms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaPayload {
    pub kind: MediaKind,
    pub mime: MimeType,
    pub bytes: Vec<u8>,
}

impl MediaPayload {
    #[must_use]
    pub fn new(kind: MediaKind, mime: MimeType, bytes: Vec<u8>) -> Self {
        Self { kind, mime, bytes }
    }
}

/// A pluggable, provider-agnostic preprocessing step such as resizing an image,
/// transcoding audio or trimming a video.
///
/// The framework ships no policy and knows no model limits; a host configures
/// one and hands it to the engine. Implementations should be idempotent so they
/// can be applied on every turn without shrinking repeatedly.
#[async_trait::async_trait]
pub trait MediaPreprocessor: Send + Sync {
    /// # Errors
    ///
    /// Returns an error when the payload cannot be decoded or re-encoded.
    async fn preprocess(&self, payload: MediaPayload) -> Result<MediaPayload>;
}
