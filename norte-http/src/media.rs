use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use norte_core::{
    BlobStore, Error, MediaAdapter, MediaKind, MediaPart, MediaSource, MimeType, ProviderError,
    ProviderErrorKind, ProviderFileRef, ProviderFiles, Result,
};

use crate::client::HttpClient;

/// The default media port, shared by the `HTTP` adapters.
///
/// On output it durable-izes any `URL` or provider file the vendor returned, so
/// the graph never depends on an expiring link or a vendor-side file (FR-PART-5,
/// FR-PART-4). On input it reads a stored blob back into an inline payload the
/// request mapper encodes for its wire. Reading the same blob always yields the
/// same bytes, so the cached prefix stays stable (FR-RECON-4).
///
/// A provider file that has expired is re-uploaded from its fallback blob
/// through the attached [`ProviderFiles`] port. The fresh reference is memoized
/// so repeated reconstruction is byte-stable instead of minting a new vendor id
/// on every turn (FR-RECON-3).
pub struct HttpMedia {
    client: reqwest::Client,
    files: Option<Arc<dyn ProviderFiles>>,
    restored: Arc<Mutex<BTreeMap<(String, String), ProviderFileRef>>>,
}

impl Clone for HttpMedia {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            files: self.files.clone(),
            restored: Arc::clone(&self.restored),
        }
    }
}

impl std::fmt::Debug for HttpMedia {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpMedia")
            .field("provider_files", &self.files.is_some())
            .finish_non_exhaustive()
    }
}

impl HttpMedia {
    #[must_use]
    pub fn new(client: &HttpClient) -> Self {
        Self {
            client: client.raw().clone(),
            files: None,
            restored: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    /// Attaches the vendor port that downloads and re-uploads provider files
    /// (FR-PART-4, FR-PART-5).
    #[must_use]
    pub fn with_provider_files(mut self, files: Arc<dyn ProviderFiles>) -> Self {
        self.files = Some(files);
        self
    }

    async fn fetch(&self, url: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| transport(&error))?;
        if !response.status().is_success() {
            return Err(Error::provider(ProviderError::new(
                ProviderErrorKind::Transport,
                format!(
                    "media fetch failed with status {}",
                    response.status().as_u16()
                ),
            )));
        }
        let bytes = response.bytes().await.map_err(|error| transport(&error))?;
        Ok(bytes.to_vec())
    }

    async fn download(&self, reference: &ProviderFileRef) -> Result<Vec<u8>> {
        let files = self
            .files
            .as_deref()
            .ok_or_else(|| unavailable(reference, "no provider-file port is configured"))?;
        files.download(reference).await
    }

    /// Revives an expired reference from its fallback blob, memoizing the fresh
    /// reference so repeated reconstruction stays byte-stable (FR-RECON-3).
    async fn revive(
        &self,
        reference: &ProviderFileRef,
        kind: MediaKind,
        mime: &MimeType,
        blobs: &dyn BlobStore,
    ) -> Result<ProviderFileRef> {
        let key = (
            reference.provider.as_str().to_owned(),
            reference.file_id.clone(),
        );
        if let Some(cached) = self.cached(&key) {
            return Ok(cached);
        }
        let Some(blob) = reference.blob else {
            return Err(unavailable(
                reference,
                "the reference carries no fallback blob",
            ));
        };
        if !blobs.contains(blob).await? {
            return Err(unavailable(reference, "the fallback blob is gone"));
        }
        let files = self
            .files
            .as_deref()
            .ok_or_else(|| unavailable(reference, "no provider-file port is configured"))?;
        let bytes = blobs.get(blob).await?;
        let fresh = files.upload(kind, mime, &bytes).await?;
        self.remember(key, fresh.clone());
        Ok(fresh)
    }

    fn cached(&self, key: &(String, String)) -> Option<ProviderFileRef> {
        self.restored.lock().expect("media cache").get(key).cloned()
    }

    fn remember(&self, key: (String, String), reference: ProviderFileRef) {
        self.restored
            .lock()
            .expect("media cache")
            .insert(key, reference);
    }
}

#[async_trait::async_trait]
impl MediaAdapter for HttpMedia {
    async fn ingest(&self, mut part: MediaPart, blobs: &dyn BlobStore) -> Result<MediaPart> {
        let bytes = match &part.source {
            MediaSource::Url(url) => self.fetch(url).await?,
            MediaSource::ProviderFile(reference) => self.download(reference).await?,
            _ => return Ok(part),
        };
        let id = blobs.put(&bytes).await?;
        part.source = MediaSource::Blob(id);
        Ok(part)
    }

    async fn resolve(&self, mut part: MediaPart, blobs: &dyn BlobStore) -> Result<MediaPart> {
        let resolved = match &part.source {
            MediaSource::Blob(id) => Some(MediaSource::Inline(blobs.get(*id).await?)),
            MediaSource::ProviderFile(reference) if reference.is_expired() => {
                let fresh = self.revive(reference, part.kind, &part.mime, blobs).await?;
                Some(MediaSource::ProviderFile(fresh))
            }
            _ => None,
        };
        if let Some(source) = resolved {
            part.source = source;
        }
        Ok(part)
    }
}

fn unavailable(reference: &ProviderFileRef, reason: &str) -> Error {
    Error::ProviderFileUnavailable {
        provider: reference.provider.clone(),
        file_id: reference.file_id.clone(),
        reason: reason.to_owned(),
    }
}

fn transport(error: &reqwest::Error) -> Error {
    Error::provider(ProviderError::new(
        ProviderErrorKind::Transport,
        error.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use norte_core::{BlobId, ProviderId};

    use super::*;

    #[derive(Default)]
    struct MemoryBlobs {
        inner: Mutex<BTreeMap<BlobId, Vec<u8>>>,
    }

    #[async_trait::async_trait]
    impl BlobStore for MemoryBlobs {
        async fn put(&self, bytes: &[u8]) -> Result<BlobId> {
            let id = BlobId::of(bytes);
            self.inner.lock().expect("lock").insert(id, bytes.to_vec());
            Ok(id)
        }

        async fn get(&self, id: BlobId) -> Result<Vec<u8>> {
            self.inner
                .lock()
                .expect("lock")
                .get(&id)
                .cloned()
                .ok_or(Error::BlobNotFound(id))
        }

        async fn contains(&self, id: BlobId) -> Result<bool> {
            Ok(self.inner.lock().expect("lock").contains_key(&id))
        }
    }

    struct FakeFiles {
        bytes: Vec<u8>,
        uploads: Mutex<u32>,
    }

    impl FakeFiles {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                uploads: Mutex::new(0),
            }
        }

        fn uploads(&self) -> u32 {
            *self.uploads.lock().expect("lock")
        }
    }

    #[async_trait::async_trait]
    impl ProviderFiles for FakeFiles {
        async fn download(&self, _reference: &ProviderFileRef) -> Result<Vec<u8>> {
            Ok(self.bytes.clone())
        }

        async fn upload(
            &self,
            _kind: MediaKind,
            _mime: &MimeType,
            bytes: &[u8],
        ) -> Result<ProviderFileRef> {
            let mut uploads = self.uploads.lock().expect("lock");
            *uploads += 1;
            Ok(ProviderFileRef {
                provider: ProviderId::new("test").expect("provider"),
                file_id: format!("uploaded-{}", *uploads),
                expires_at: None,
                blob: Some(BlobId::of(bytes)),
            })
        }
    }

    fn client() -> HttpClient {
        HttpClient::new().expect("client")
    }

    fn mime() -> MimeType {
        MimeType::new("application/pdf").expect("mime")
    }

    fn reference(
        file_id: &str,
        expires_at: Option<DateTime<Utc>>,
        blob: Option<BlobId>,
    ) -> ProviderFileRef {
        ProviderFileRef {
            provider: ProviderId::new("test").expect("provider"),
            file_id: file_id.to_owned(),
            expires_at,
            blob,
        }
    }

    fn expired() -> DateTime<Utc> {
        DateTime::from_timestamp(0, 0).expect("timestamp")
    }

    fn provider_file(
        file_id: &str,
        expires_at: Option<DateTime<Utc>>,
        blob: Option<BlobId>,
    ) -> MediaPart {
        MediaPart::new(
            MediaKind::Document,
            MediaSource::ProviderFile(reference(file_id, expires_at, blob)),
            mime(),
        )
    }

    #[tokio::test]
    async fn a_provider_file_output_is_ingested_into_a_blob() {
        let blobs = MemoryBlobs::default();
        let media = HttpMedia::new(&client()).with_provider_files(Arc::new(FakeFiles::new(b"doc")));

        let ingested = media
            .ingest(provider_file("vendor-1", None, None), &blobs)
            .await
            .expect("ingest");

        assert_eq!(ingested.source, MediaSource::Blob(BlobId::of(b"doc")));
    }

    #[tokio::test]
    async fn a_stored_blob_resolves_to_inline_bytes() {
        let blobs = MemoryBlobs::default();
        let id = blobs.put(b"abc").await.expect("put");
        let media = HttpMedia::new(&client());

        let part = MediaPart::new(MediaKind::Image, MediaSource::Blob(id), mime());
        let resolved = media.resolve(part, &blobs).await.expect("resolve");

        assert_eq!(resolved.source, MediaSource::Inline(b"abc".to_vec()));
    }

    #[tokio::test]
    async fn a_fresh_provider_file_is_forwarded_untouched() {
        let blobs = MemoryBlobs::default();
        let media = HttpMedia::new(&client()).with_provider_files(Arc::new(FakeFiles::new(b"x")));

        let part = provider_file("fresh", None, None);
        let resolved = media.resolve(part.clone(), &blobs).await.expect("resolve");

        assert_eq!(resolved.source, part.source);
    }

    #[tokio::test]
    async fn an_expired_provider_file_is_revived_from_its_blob_once() {
        let blobs = MemoryBlobs::default();
        let blob = blobs.put(b"doc").await.expect("put");
        let files = Arc::new(FakeFiles::new(b"doc"));
        let media = HttpMedia::new(&client()).with_provider_files(files.clone());

        let part = provider_file("old", Some(expired()), Some(blob));
        let first = media.resolve(part.clone(), &blobs).await.expect("first");
        let second = media.resolve(part, &blobs).await.expect("second");

        assert_eq!(first.source, second.source);
        assert_eq!(files.uploads(), 1);
        let MediaSource::ProviderFile(fresh) = &first.source else {
            panic!("expected a provider file");
        };
        assert_ne!(fresh.file_id, "old");
    }

    #[tokio::test]
    async fn an_expired_provider_file_without_a_blob_is_a_typed_error() {
        let blobs = MemoryBlobs::default();
        let media = HttpMedia::new(&client()).with_provider_files(Arc::new(FakeFiles::new(b"x")));

        let error = media
            .resolve(provider_file("gone", Some(expired()), None), &blobs)
            .await
            .expect_err("typed error");

        assert!(matches!(error, Error::ProviderFileUnavailable { .. }));
    }

    #[tokio::test]
    async fn an_expired_provider_file_whose_blob_is_gone_is_a_typed_error() {
        let blobs = MemoryBlobs::default();
        let media = HttpMedia::new(&client()).with_provider_files(Arc::new(FakeFiles::new(b"x")));

        let error = media
            .resolve(
                provider_file("gone", Some(expired()), Some(BlobId::of(b"never stored"))),
                &blobs,
            )
            .await
            .expect_err("typed error");

        assert!(matches!(error, Error::ProviderFileUnavailable { .. }));
    }
}
