//! The blob port: content-addressed bytes, for media and for anything else too
//! large to travel inline.
//!
//! This stays in the core because [`MediaAdapter`](crate::media_adapter::MediaAdapter)
//! names it and `norte-http::HttpMedia` implements that port: moving the blob
//! store out would point an adapter at `norte-harness`.
//!
//! The *session* store is a different question. That one exists because there is
//! a runtime that commits turns and parks approvals, and it lives in
//! `norte-harness` with the runtime.

use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::error::{Error, Result};
use crate::id::BlobId;

/// Content-addressed byte storage.
#[async_trait::async_trait]
pub trait BlobStore: Send + Sync {
    /// Idempotent: the same bytes always produce the same id.
    async fn put(&self, bytes: &[u8]) -> Result<BlobId>;

    /// The bytes stored under `id`, or a typed error when it is absent.
    async fn get(&self, id: BlobId) -> Result<Vec<u8>>;

    /// Whether `id` is present, without reading its bytes.
    async fn contains(&self, id: BlobId) -> Result<bool>;
}

/// A content-addressed in-memory [`BlobStore`]; puts are idempotent.
#[derive(Default)]
pub struct InMemoryBlobStore {
    blobs: RwLock<HashMap<BlobId, Vec<u8>>>,
}

impl InMemoryBlobStore {
    /// Creates an empty blob store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of distinct blobs held.
    pub async fn len(&self) -> usize {
        self.blobs.read().await.len()
    }

    /// Whether no blob is held.
    pub async fn is_empty(&self) -> bool {
        self.blobs.read().await.is_empty()
    }
}

#[async_trait::async_trait]
impl BlobStore for InMemoryBlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobId> {
        let id = BlobId::of(bytes);
        self.blobs
            .write()
            .await
            .entry(id)
            .or_insert_with(|| bytes.to_vec());
        Ok(id)
    }

    async fn get(&self, id: BlobId) -> Result<Vec<u8>> {
        self.blobs
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(Error::BlobNotFound(id))
    }

    async fn contains(&self, id: BlobId) -> Result<bool> {
        Ok(self.blobs.read().await.contains_key(&id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn blobs_are_content_addressed() {
        let blobs = InMemoryBlobStore::new();
        let first = blobs.put(b"hello").await.expect("put");
        let second = blobs.put(b"hello").await.expect("put");
        assert_eq!(first, second);
        assert_eq!(blobs.get(first).await.expect("get"), b"hello");
        assert!(blobs.contains(first).await.expect("contains"));
        assert_eq!(blobs.len().await, 1);
        assert!(matches!(
            blobs.get(BlobId::of(b"absent")).await,
            Err(Error::BlobNotFound(_))
        ));
    }
}
