use crate::blob::BlobStore;
use crate::domain::media::{MediaKind, MediaPart, MimeType, ProviderFileRef};
use crate::error::Result;

/// The seam where media meets a vendor wire format.
///
/// Core owns the graph and calls this during reconstruction and on model output;
/// adapters own HTTP access and vendor knowledge, so they implement it. Both
/// directions must be deterministic so the provider prompt cache keeps hitting
/// (FR-RECON-4): resolving the same stored part must produce the same wire form.
#[async_trait::async_trait]
pub trait MediaAdapter: Send + Sync {
    /// Durable-izes model output before it is committed: fetch a URL or vendor
    /// file into the blob store so the graph never depends on an ephemeral,
    /// expiring URL (FR-PART-5).
    ///
    /// # Errors
    ///
    /// Returns an error when the media cannot be persisted.
    async fn ingest(&self, part: MediaPart, blobs: &dyn BlobStore) -> Result<MediaPart>;

    /// Produces the wire form of a stored media part for a request (FR-RECON-4).
    ///
    /// # Errors
    ///
    /// Returns an error when a part cannot be rebuilt, for example an expired
    /// vendor reference whose blob is gone (FR-PART-4).
    async fn resolve(&self, part: MediaPart, blobs: &dyn BlobStore) -> Result<MediaPart>;
}

/// The vendor half of the `ProviderFile` lifecycle (FR-PART-4, FR-PART-5).
///
/// A wire adapter implements it: [`download`](ProviderFiles::download) fetches a
/// vendor file's bytes so an output reference can be durable-ized into the blob
/// store, and [`upload`](ProviderFiles::upload) stores bytes and returns a fresh
/// reference so an expired one can be revived from its blob. Implementations
/// must be deterministic for the same bytes so the reconstructed prefix stays
/// stable (FR-RECON-3, FR-RECON-4).
#[async_trait::async_trait]
pub trait ProviderFiles: Send + Sync {
    /// Downloads the bytes a vendor file reference points at.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be fetched.
    async fn download(&self, reference: &ProviderFileRef) -> Result<Vec<u8>>;

    /// Uploads bytes and returns a fresh vendor file reference.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be uploaded.
    async fn upload(
        &self,
        kind: MediaKind,
        mime: &MimeType,
        bytes: &[u8],
    ) -> Result<ProviderFileRef>;
}
