use std::collections::HashMap;

use rustychain_core::{BlobId, BlobStore, Error, Result};
use tokio::sync::RwLock;

/// Content-addressed in-memory [`BlobStore`]. `put` is idempotent.
#[derive(Default)]
pub struct InMemoryBlobStore {
    blobs: RwLock<HashMap<BlobId, Vec<u8>>>,
}

impl InMemoryBlobStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub async fn len(&self) -> usize {
        self.blobs.read().await.len()
    }

    #[must_use]
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
