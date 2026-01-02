use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const DEFAULT_COLLECTION_NAME: &str = "rag_documents";
pub const DEFAULT_DIMENSIONS: usize = 1536;

/// Represents a document stored in the vector database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    /// Unique identifier for the document (String for universal compatibility)
    pub id: String,
    /// The text content of the document
    pub text: String,
    /// Optional collection/namespace for organizing documents
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Optional metadata as arbitrary JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    /// The embedding vector (only included when explicitly requested)
    #[serde(skip)]
    pub embedding: Option<Vec<f32>>,
}

/// Input for adding a new document
#[derive(Debug, Clone)]
pub struct DocumentInput {
    pub text: String,
    pub embedding: Vec<f32>,
    pub collection: Option<String>,
    pub metadata: Option<Value>,
}

impl DocumentInput {
    pub fn new(text: String, embedding: Vec<f32>) -> Self {
        Self {
            text,
            embedding,
            collection: None,
            metadata: None,
        }
    }

    pub fn with_collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    pub fn with_metadata(mut self, metadata: Value) -> Self {
        self.metadata = Some(metadata);
        self
    }
}

/// A search result containing the document and similarity metrics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// The retrieved document
    pub document: Document,
    /// Distance metric (lower is more similar for most metrics)
    pub distance: f32,
    /// Similarity score (higher is more similar, typically 1.0 - distance)
    pub score: f32,
}

/// Options for configuring vector similarity search
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Default)]
pub struct SearchOptions {
    /// Number of nearest neighbors to retrieve
    #[schemars(description = "Number of nearest neighbors to retrieve")]
    pub k: usize,
    /// Distance threshold for filtering results (0.0 to 2.0)
    #[schemars(
        description = "Distance threshold for filtering results",
        range(min = 0.0, max = 2.0)
    )]
    pub threshold: Option<f32>,
    /// Filter by collection name
    #[schemars(
        description = "The collection name to filter documents. If set, only documents from this collection will be considered."
    )]
    pub collection: Option<String>,
    /// Filter by metadata fields (key-value pairs)
    #[schemars(
        description = "Metadata filter as a JSON object. The filter should be a JSON object where keys are metadata fields and values are the expected values."
    )]
    pub metadata_filter: Option<Value>,
    /// Whether to include embeddings in the results
    #[schemars(
        description = "Whether to include embeddings in the results. Embeddings can be large, so enable only if needed."
    )]
    pub include_embeddings: bool,
    /// Whether to include distances in the results
    #[schemars(description = "Whether to include distances in the results.")]
    pub include_distances: bool,
}

impl SearchOptions {
    pub fn new(k: usize) -> Self {
        Self {
            k,
            ..Default::default()
        }
    }

    pub fn with_threshold(mut self, threshold: f32) -> Self {
        self.threshold = Some(threshold);
        self
    }

    pub fn with_collection(mut self, collection: impl Into<String>) -> Self {
        self.collection = Some(collection.into());
        self
    }

    pub fn with_metadata_filter(mut self, filter: Value) -> Self {
        self.metadata_filter = Some(filter);
        self
    }

    pub fn include_embeddings(mut self) -> Self {
        self.include_embeddings = true;
        self
    }

    pub fn include_distances(mut self) -> Self {
        self.include_distances = true;
        self
    }
}

/// Statistics about a collection or the entire vector store
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionStats {
    /// Total number of documents/vectors stored
    pub total_documents: i64,
    /// Total size in bytes (if applicable)
    pub total_size_bytes: Option<i64>,
    /// Additional backend-specific metadata
    #[serde(flatten)]
    pub metadata: Option<Value>,
}

/// Pagination cursor for listing documents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cursor {
    /// Offset-based cursor (for SQL databases)
    pub offset: Option<i64>,
    /// ID-based cursor (for document databases)
    pub last_id: Option<String>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            offset: Some(0),
            last_id: None,
        }
    }
}

/// Trait for vector store implementations
#[async_trait::async_trait]
pub trait VectorStore: Send + Sync {
    /// Get the dimensionality of vectors in this store
    fn dims(&self) -> usize;

    /// Add a single document with its embedding
    ///
    /// Returns the ID of the added document
    async fn add_document(
        &self,
        text: String,
        embedding: Vec<f32>,
        collection: Option<String>,
        metadata: Option<Value>,
    ) -> crate::Result<String>;

    /// Add multiple documents in a batch (more efficient than individual adds)
    ///
    /// Returns the IDs of all added documents
    async fn add_documents_batch(
        &self,
        documents: Vec<DocumentInput>,
    ) -> crate::Result<Vec<String>> {
        let mut ids = Vec::new();
        for doc in documents {
            let id = self
                .add_document(doc.text, doc.embedding, doc.collection, doc.metadata)
                .await?;
            ids.push(id);
        }
        Ok(ids)
    }

    /// Perform similarity search with advanced options
    async fn search(
        &self,
        query_embedding: Vec<f32>,
        options: SearchOptions,
    ) -> crate::Result<Vec<SearchResult>>;

    /// Simple similarity search returning top k results
    ///
    /// This is a convenience wrapper around `search()` with default options.
    async fn similarity_search(
        &self,
        query_embedding: Vec<f32>,
        k: usize,
    ) -> crate::Result<Vec<SearchResult>> {
        self.search(query_embedding, SearchOptions::new(k).include_distances())
            .await
    }

    /// Simple similarity search returning results as concatenated text
    ///
    /// Useful for RAG contexts where you need a single string of relevant documents.
    ///
    /// # Example
    /// ```ignore
    /// let context = store.similarity_search_text(embedding, 5).await?;
    /// let prompt = format!("Context:\n{}\n\nQuestion: {}", context, question);
    /// ```
    async fn similarity_search_text(
        &self,
        query_embedding: Vec<f32>,
        k: usize,
    ) -> crate::Result<String> {
        let results = self.search(query_embedding, SearchOptions::new(k)).await?;

        if results.is_empty() {
            return Err(crate::Error::NotFound(
                "No relevant documents found.".to_string(),
            ));
        }

        let context = results
            .into_iter()
            .map(|r| r.document.text)
            .collect::<Vec<_>>()
            .join("\n---\n");

        Ok(context)
    }

    /// Delete a document by its ID
    ///
    /// Returns true if the document was found and deleted
    async fn delete(&self, id: &str) -> crate::Result<bool>;

    /// Delete all documents in a specific collection
    ///
    /// Returns the number of documents deleted
    async fn delete_collection(&self, collection: &str) -> crate::Result<u64>;

    /// Clear all documents from the store
    async fn clear(&self) -> crate::Result<()>;

    /// Get the total count of documents
    ///
    /// Optionally filter by collection
    async fn count(&self, collection: Option<&str>) -> crate::Result<i64>;

    /// List all collection names in the store
    async fn list_collections(&self) -> crate::Result<Vec<String>>;

    /// Rebuild/optimize indices (implementation-specific)
    ///
    /// Some backends may not need this and can use the default no-op implementation
    async fn reindex(&self) -> crate::Result<()> {
        Ok(())
    }

    /// Get statistics about the store or a specific collection
    async fn stats(&self, collection: Option<&str>) -> crate::Result<CollectionStats>;

    /// Delete documents matching a metadata filter
    ///
    /// Returns the number of documents deleted
    async fn delete_by_metadata(
        &self,
        filter: &Value,
        collection: Option<&str>,
    ) -> crate::Result<u64>;

    /// List documents with pagination
    ///
    /// Use cursor for offset or ID-based pagination depending on backend
    async fn list(
        &self,
        limit: usize,
        cursor: Option<Cursor>,
        collection: Option<&str>,
    ) -> crate::Result<(Vec<Document>, Option<Cursor>)>;

    /// Get a specific document by ID
    async fn get(&self, id: &str) -> crate::Result<Option<Document>> {
        let results = self
            .list(
                1,
                Some(Cursor {
                    offset: None,
                    last_id: Some(id.to_string()),
                }),
                None,
            )
            .await?;
        Ok(results.0.into_iter().next())
    }
}
