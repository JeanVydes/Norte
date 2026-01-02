//! MongoDB
//!
//! This module provides tools for integrating MongoDB Atlas Vector Search
//! as a vector store.

use rustychain::llm::LLM;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use rustychain::{FunctionDeclaration, RecursiveCharacterTextSplitter, TextSplitter};
use rustychain::{SearchOptions, VectorStore, prelude::*};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct SimpleRetrievalArgs {
    #[schemars(description = "The query string to search for in the MongoDB vector database.")]
    pub query: String,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ComplexRetrievalArgs {
    #[schemars(description = "The query string to search for in the MongoDB vector database.")]
    pub query: String,
    #[schemars(description = "Advanced search configuration options.")]
    pub config: SearchOptions,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct AugmentedArgs {
    #[schemars(description = "The text content to be embedded and stored in MongoDB.")]
    pub text: String,
    #[schemars(
        description = "Optional metadata to associate with the document, as a JSON object."
    )]
    pub metadata: Option<serde_json::Value>,
    #[schemars(
        description = "The name of the collection to add the document to. Use different collections for granular data categorization."
    )]
    pub collection: Option<String>,
}

#[derive(Clone)]
pub struct MongoRetrievalTool {
    pub store: Arc<Mutex<dyn VectorStore>>,
    pub llm: Arc<LLM>,
    pub collection_name: Option<String>,
}

#[derive(Clone)]
pub struct MongoAugmentedTool {
    pub store: Arc<Mutex<dyn VectorStore>>,
    pub llm: Arc<LLM>,
    pub collection_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalResult {
    pub query: String,
    pub results: Vec<(String, Option<serde_json::Value>)>,
}

#[async_trait::async_trait]
impl FnExecutor<SimpleRetrievalArgs, RetrievalResult> for MongoRetrievalTool {
    async fn call(&self, args: SimpleRetrievalArgs) -> rustychain::Result<RetrievalResult> {
        let query = self.llm.embedding(&args.query, 1536).await?;
        let store = self.store.lock().await;
        let results = store.similarity_search(query, 10).await?;

        Ok(RetrievalResult {
            query: args.query,
            results: results
                .into_iter()
                .map(|r| (r.document.text, r.document.metadata))
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl FnExecutor<ComplexRetrievalArgs, RetrievalResult> for MongoRetrievalTool {
    async fn call(&self, args: ComplexRetrievalArgs) -> rustychain::Result<RetrievalResult> {
        let query = self.llm.embedding(&args.query, 1536).await?;
        let store = self.store.lock().await;
        let context = store.search(query, args.config).await?;

        Ok(RetrievalResult {
            query: args.query,
            results: context
                .into_iter()
                .map(|r| (r.document.text, r.document.metadata))
                .collect(),
        })
    }
}

#[async_trait::async_trait]
impl FnExecutor<AugmentedArgs, serde_json::Value> for MongoAugmentedTool {
    async fn call(&self, args: AugmentedArgs) -> rustychain::Result<serde_json::Value> {
        let splitter = RecursiveCharacterTextSplitter::new(rustychain::SplitterConfig {
            chunk_size: 1024,
            chunk_overlap: 256,
            trim_chunks: true,
            track_indices: true,
            min_chunk_size: None,
        });

        let chunks = splitter.split(&args.text);
        let mut documents = Vec::new();

        for chunk in chunks {
            let embedding = self.llm.embedding(&chunk.content, 1536).await?;
            documents.push(rustychain::DocumentInput {
                text: chunk.content,
                embedding,
                metadata: args.metadata.clone(),
                collection: args.collection.clone(),
            });
        }

        let chunks_added = documents.len();

        let store = self.store.lock().await;
        store.add_documents_batch(documents).await?;

        Ok(serde_json::json!({
            "status": "Success",
            "chunks_added": chunks_added,
        }))
    }
}

impl FnDeclarator<SimpleRetrievalArgs, RetrievalResult> for MongoRetrievalTool {
    fn declare(&self) -> FunctionDeclaration<SimpleRetrievalArgs, RetrievalResult> {
        FunctionDeclaration {
            name: "mongo_retrieval_tool",
            description: "Search for semantically similar documents in MongoDB Atlas. Useful for answering questions based on stored knowledge.",
            parameters: schema_for!(SimpleRetrievalArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

impl FnDeclarator<ComplexRetrievalArgs, RetrievalResult> for MongoRetrievalTool {
    fn declare(&self) -> FunctionDeclaration<ComplexRetrievalArgs, RetrievalResult> {
        FunctionDeclaration {
            name: "mongo_complex_retrieval_tool",
            description: "Advanced similarity search in MongoDB with filtering capabilities (metadata, collection names, and score thresholds).",
            parameters: schema_for!(ComplexRetrievalArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

impl FnDeclarator<AugmentedArgs, serde_json::Value> for MongoAugmentedTool {
    fn declare(&self) -> FunctionDeclaration<AugmentedArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "mongo_augmented_tool",
            description: "Save new information into the MongoDB vector store. The tool handles chunking and embedding generation automatically.",
            parameters: schema_for!(AugmentedArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
