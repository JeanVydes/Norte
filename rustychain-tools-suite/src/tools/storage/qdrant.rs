//! Qdrant
//!
//! This module provides tools for integrating Qdrant
//! as a vector store.

use rustychain::llm::LLM;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use rustychain::{FunctionDeclaration, RecursiveCharacterTextSplitter, TextSplitter};
use rustychain::{SearchOptions, VectorStore, prelude::*};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct SimpleRetrievalArgs {
    #[schemars(description = "The query string to search for in the vector database.")]
    pub query: String,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ComplexRetrievalArgs {
    #[schemars(description = "The query string to search for in the vector database.")]
    pub query: String,
    #[schemars(description = "Advanced search configuration options.")]
    pub config: SearchOptions,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct AugmentedArgs {
    #[schemars(description = "The text content to be embedded and stored in the vector database.")]
    pub text: String,
    #[schemars(
        description = "Optional metadata to associate with the document, as a JSON object."
    )]
    pub metadata: Option<serde_json::Value>,
    #[schemars(
        description = "The name of the collection to add the document to. It is recommended to use different collections for granular data. Example: all data related to invoices of January 2024 Q1 can go into 'invoices_jan_2024_q1' collection."
    )]
    pub collection: Option<String>,
}

#[derive(Clone)]
pub struct QdrantRetrievalTool {
    pub store: Arc<Mutex<dyn VectorStore>>,
    pub llm: Arc<LLM>,
    pub collection_name: Option<String>,
}

#[derive(Clone)]
pub struct QdrantAugmentedTool {
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
impl FnExecutor<SimpleRetrievalArgs, RetrievalResult> for QdrantRetrievalTool {
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
impl FnExecutor<ComplexRetrievalArgs, RetrievalResult> for QdrantRetrievalTool {
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
impl FnExecutor<AugmentedArgs, serde_json::Value> for QdrantAugmentedTool {
    async fn call(&self, args: AugmentedArgs) -> rustychain::Result<serde_json::Value> {
        let splitter = RecursiveCharacterTextSplitter::new(rustychain::SplitterConfig {
            chunk_size: 1024,
            chunk_overlap: 256,
            trim_chunks: true,
            track_indices: true,
            min_chunk_size: None,
        });

        let chunks = splitter.split(&args.text);

        let mut embeddings: HashMap<String, Vec<f32>> = HashMap::new();

        for chunk in chunks {
            let embedding = self.llm.embedding(&chunk.content, 1536).await?;
            embeddings.insert(chunk.content.clone(), embedding);
        }

        let store = self.store.lock().await;
        let documents = embeddings
            .into_iter()
            .map(|(text, embedding)| rustychain::DocumentInput {
                text,
                embedding,
                metadata: args.metadata.clone(),
                collection: args.collection.clone(),
            })
            .collect::<Vec<_>>();

        store.add_documents_batch(documents).await?;
        
        Ok(serde_json::json!({
            "status": "Document added successfully"
        }))
    }
}

impl FnDeclarator<SimpleRetrievalArgs, RetrievalResult> for QdrantRetrievalTool {
    fn declare(&self) -> FunctionDeclaration<SimpleRetrievalArgs, RetrievalResult> {
        FunctionDeclaration {
            name: "qdrant_retrieval_tool",
            description: "Use this tool to perform a similarity search in the Qdrant vector store and retrieve relevant documents based on the user's query.",
            parameters: schema_for!(SimpleRetrievalArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

impl FnDeclarator<ComplexRetrievalArgs, RetrievalResult> for QdrantRetrievalTool {
    fn declare(&self) -> FunctionDeclaration<ComplexRetrievalArgs, RetrievalResult> {
        FunctionDeclaration {
            name: "qdrant_complex_retrieval_tool",
            description: "Use this tool to perform a similarity search in the Qdrant vector store with advanced search options and retrieve relevant documents based on the user's query.",
            parameters: schema_for!(ComplexRetrievalArgs),
            executor: Arc::new(QdrantRetrievalTool {
                store: self.store.clone(),
                llm: self.llm.clone(),
                collection_name: self.collection_name.clone(),
            }),
        }
    }
}

impl FnDeclarator<AugmentedArgs, serde_json::Value> for QdrantAugmentedTool {
    fn declare(&self) -> FunctionDeclaration<AugmentedArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "qdrant_augmented_tool",
            description: "Use this tool to add a new document to the Qdrant vector store with its corresponding embedding. The document will be automatically split into chunks if it's too large.",
            parameters: schema_for!(AugmentedArgs),
            executor: Arc::new(QdrantAugmentedTool {
                store: self.store.clone(),
                llm: self.llm.clone(),
                collection_name: self.collection_name.clone(),
            }),
        }
    }
}
