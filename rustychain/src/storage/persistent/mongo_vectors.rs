//! mongodb for vector storage

use futures_util::stream::StreamExt;
use mongodb::{
    Client, Collection, IndexModel,
    bson::{Document as BsonDocument, doc, oid::ObjectId},
    options::ClientOptions,
};
use serde_json::Value;

use crate::{
    CollectionStats, Cursor, DEFAULT_COLLECTION_NAME, DEFAULT_DIMENSIONS, Document, SearchOptions,
    SearchResult, VectorStore,
};

/// MongoDB Atlas Vector Search implementation of VectorStore
#[derive(Clone)]
pub struct MongoDBStore {
    pub client: Client,
    pub database_name: String,
    pub collection_name: Option<String>,
    pub dimensions: Option<usize>,
    pub index_name: String,
}

impl MongoDBStore {
    pub async fn new(
        connection_string: &str,
        database_name: impl Into<String>,
        collection_name: Option<String>,
        dimensions: Option<usize>,
    ) -> crate::Result<Self> {
        let client_options = ClientOptions::parse(connection_string).await?;
        let client = Client::with_options(client_options)?;

        let store = MongoDBStore {
            client,
            database_name: database_name.into(),
            collection_name,
            dimensions,
            index_name: "vector_index".to_string(),
        };

        store.ensure_collection_exists().await?;
        Ok(store)
    }

    pub fn with_index_name(mut self, index_name: impl Into<String>) -> Self {
        self.index_name = index_name.into();
        self
    }

    fn collection_name(&self) -> &str {
        self.collection_name
            .as_deref()
            .unwrap_or(DEFAULT_COLLECTION_NAME)
    }

    fn collection(&self) -> Collection<BsonDocument> {
        self.client
            .database(&self.database_name)
            .collection(self.collection_name())
    }

    /// Ensure the collection exists and has proper indices
    async fn ensure_collection_exists(&self) -> crate::Result<()> {
        let collection = self.collection();

        // Create text index for the text field
        let text_index = IndexModel::builder().keys(doc! { "text": 1 }).build();

        // Create index for collection field
        let collection_index = IndexModel::builder().keys(doc! { "collection": 1 }).build();

        // Create compound index for metadata queries
        let metadata_index = IndexModel::builder().keys(doc! { "metadata": 1 }).build();

        collection
            .create_indexes(vec![text_index, collection_index, metadata_index])
            .await
            .ok(); // Ignore errors if indices already exist

        Ok(())
    }

    /// Convert ObjectId to string
    fn oid_to_string(oid: ObjectId) -> String {
        oid.to_hex()
    }

    /// Convert string to ObjectId
    fn string_to_oid(s: &str) -> Option<ObjectId> {
        ObjectId::parse_str(s).ok()
    }
}

#[async_trait::async_trait]
impl VectorStore for MongoDBStore {
    fn dims(&self) -> usize {
        self.dimensions.unwrap_or(DEFAULT_DIMENSIONS)
    }

    async fn add_document(
        &self,
        text: String,
        embedding: Vec<f32>,
        collection: Option<String>,
        metadata: Option<Value>,
    ) -> crate::Result<String> {
        let collection_ref = self.collection();

        let mut doc = doc! {
            "text": text,
            "embedding": embedding,
        };

        if let Some(coll) = collection {
            doc.insert("collection", coll);
        }

        if let Some(meta) = metadata {
            doc.insert("metadata", mongodb::bson::to_bson(&meta)?);
        }

        let result = collection_ref.insert_one(doc).await?;

        let id = result
            .inserted_id
            .as_object_id()
            .ok_or_else(|| crate::Error::Input("Failed to get inserted ID".to_string()))?;

        Ok(Self::oid_to_string(id))
    }

    async fn search(
        &self,
        query_embedding: Vec<f32>,
        options: SearchOptions,
    ) -> crate::Result<Vec<SearchResult>> {
        let collection_ref = self.collection();

        // Build the aggregation pipeline for vector search
        let mut pipeline = vec![doc! {
            "$vectorSearch": {
                "index": &self.index_name,
                "path": "embedding",
                "queryVector": query_embedding,
                "numCandidates": (options.k * 10) as i32, // Overquery for better results
                "limit": options.k as i32,
            }
        }];

        // Add score projection
        if options.include_distances {
            pipeline.push(doc! {
                "$addFields": {
                    "score": { "$meta": "vectorSearchScore" }
                }
            });
        }

        // Build match filters
        let mut match_conditions = Vec::new();

        if let Some(ref coll) = options.collection {
            match_conditions.push(doc! { "collection": coll });
        }

        if let Some(ref filter) = options.metadata_filter
            && let Some(obj) = filter.as_object()
        {
            for (key, value) in obj {
                let field_name = format!("metadata.{}", key);
                if let Some(v) = value.as_str() {
                    match_conditions.push(doc! { field_name: v });
                } else if let Some(v) = value.as_i64() {
                    match_conditions.push(doc! { field_name: v });
                } else if let Some(v) = value.as_bool() {
                    match_conditions.push(doc! { field_name: v });
                }
            }
        }

        if !match_conditions.is_empty() {
            pipeline.push(doc! {
                "$match": {
                    "$and": match_conditions
                }
            });
        }

        // Apply score threshold if specified
        if let Some(threshold) = options.threshold {
            pipeline.push(doc! {
                "$match": {
                    "score": { "$gte": threshold }
                }
            });
        }

        // Project fields
        let mut project = doc! {
            "_id": 1,
            "text": 1,
            "collection": 1,
            "metadata": 1,
        };

        if options.include_embeddings {
            project.insert("embedding", 1);
        }

        if options.include_distances {
            project.insert("score", 1);
        }

        pipeline.push(doc! { "$project": project });

        let mut cursor = collection_ref.aggregate(pipeline).await?;

        let mut results = Vec::new();

        while let Some(result) = cursor.next().await {
            let doc = result?;

            let id = doc
                .get_object_id("_id")
                .map(Self::oid_to_string)
                .unwrap_or_default();

            let text = doc.get_str("text").unwrap_or("").to_string();

            let collection = doc.get_str("collection").ok().map(|s| s.to_string());

            let metadata = doc.get_document("metadata").ok().and_then(|m| {
                mongodb::bson::from_bson::<Value>(mongodb::bson::Bson::Document(m.clone())).ok()
            });

            let embedding = if options.include_embeddings {
                doc.get_array("embedding").ok().and_then(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_f64().map(|f| f as f32))
                        .collect::<Vec<_>>()
                        .into()
                })
            } else {
                None
            };

            let score = if options.include_distances {
                doc.get_f64("score").unwrap_or(0.0) as f32
            } else {
                0.0
            };

            let distance = if options.include_distances {
                1.0 - score
            } else {
                0.0
            };

            results.push(SearchResult {
                document: Document {
                    id,
                    text,
                    collection,
                    metadata,
                    embedding,
                },
                distance,
                score,
            });
        }

        Ok(results)
    }

    async fn delete(&self, id: &str) -> crate::Result<bool> {
        let collection_ref = self.collection();

        let oid = Self::string_to_oid(id)
            .ok_or_else(|| crate::Error::Input(format!("Invalid ObjectId: {}", id)))?;

        let result = collection_ref.delete_one(doc! { "_id": oid }).await?;

        Ok(result.deleted_count > 0)
    }

    async fn delete_collection(&self, collection: &str) -> crate::Result<u64> {
        let collection_ref = self.collection();

        let result = collection_ref
            .delete_many(doc! { "collection": collection })
            .await?;

        Ok(result.deleted_count)
    }

    async fn clear(&self) -> crate::Result<()> {
        let collection_ref = self.collection();
        collection_ref.delete_many(doc! {}).await?;
        Ok(())
    }

    async fn count(&self, collection: Option<&str>) -> crate::Result<i64> {
        let collection_ref = self.collection();

        let filter = if let Some(coll) = collection {
            doc! { "collection": coll }
        } else {
            doc! {}
        };

        let count = collection_ref.count_documents(filter).await?;
        Ok(count as i64)
    }

    async fn list_collections(&self) -> crate::Result<Vec<String>> {
        let collection_ref = self.collection();

        let distinct = collection_ref
            .distinct("collection", doc! { "collection": { "$ne": null } })
            .await?;

        let collections = distinct
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        Ok(collections)
    }

    async fn stats(&self, collection: Option<&str>) -> crate::Result<CollectionStats> {
        let db = self.client.database(&self.database_name);

        // Get collection stats using the collStats command
        let stats_doc = db
            .run_command(doc! {
                "collStats": self.collection_name(),
                "scale": 1
            })
            .await?;

        let total_docs = if let Some(coll) = collection {
            self.count(Some(coll)).await?
        } else {
            stats_doc.get_i64("count").unwrap_or(0)
        };

        let size = stats_doc.get_i64("size").ok();
        let total_index_size = stats_doc.get_i64("totalIndexSize").ok();
        let storage_size = stats_doc.get_i64("storageSize").ok();

        Ok(CollectionStats {
            total_documents: total_docs,
            total_size_bytes: size,
            metadata: Some(serde_json::json!({
                "storage_size_bytes": storage_size,
                "total_index_size_bytes": total_index_size,
            })),
        })
    }

    async fn delete_by_metadata(
        &self,
        filter: &Value,
        collection: Option<&str>,
    ) -> crate::Result<u64> {
        let collection_ref = self.collection();

        let mut match_doc = BsonDocument::new();

        if let Some(coll) = collection {
            match_doc.insert("collection", coll);
        }

        if let Some(obj) = filter.as_object() {
            for (key, value) in obj {
                let field_name = format!("metadata.{}", key);
                if let Some(v) = value.as_str() {
                    match_doc.insert(field_name, v);
                } else if let Some(v) = value.as_i64() {
                    match_doc.insert(field_name, v);
                } else if let Some(v) = value.as_bool() {
                    match_doc.insert(field_name, v);
                }
            }
        }

        let result = collection_ref.delete_many(match_doc).await?;

        Ok(result.deleted_count)
    }

    async fn list(
        &self,
        limit: usize,
        cursor: Option<Cursor>,
        collection: Option<&str>,
    ) -> crate::Result<(Vec<Document>, Option<Cursor>)> {
        let collection_ref = self.collection();

        let mut filter = BsonDocument::new();

        if let Some(coll) = collection {
            filter.insert("collection", coll);
        }

        // Handle cursor-based pagination
        if let Some(c) = &cursor
            && let Some(last_id) = &c.last_id
            && let Some(oid) = Self::string_to_oid(last_id)
        {
            filter.insert("_id", doc! { "$gt": oid });
        }

        let mut find_cursor = collection_ref.find(filter).await?;

        let mut documents = Vec::new();
        let mut last_id = None;

        while let Some(result) = find_cursor.next().await {
            let doc = result?;

            let id = doc
                .get_object_id("_id")
                .map(Self::oid_to_string)
                .unwrap_or_default();

            last_id = Some(id.clone());

            let text = doc.get_str("text").unwrap_or("").to_string();

            let collection = doc.get_str("collection").ok().map(|s| s.to_string());

            let metadata = doc.get_document("metadata").ok().and_then(|m| {
                mongodb::bson::from_bson::<Value>(mongodb::bson::Bson::Document(m.clone())).ok()
            });

            documents.push(Document {
                id,
                text,
                collection,
                metadata,
                embedding: None,
            });
        }

        let next_cursor = if documents.len() == limit {
            last_id.map(|id| Cursor {
                offset: None,
                last_id: Some(id),
            })
        } else {
            None
        };

        Ok((documents, next_cursor))
    }

    async fn get(&self, id: &str) -> crate::Result<Option<Document>> {
        let collection_ref = self.collection();

        let oid = Self::string_to_oid(id)
            .ok_or_else(|| crate::Error::Input(format!("Invalid ObjectId: {}", id)))?;

        let doc = collection_ref.find_one(doc! { "_id": oid }).await?;

        if let Some(doc) = doc {
            let id = doc
                .get_object_id("_id")
                .map(Self::oid_to_string)
                .unwrap_or_default();

            let text = doc.get_str("text").unwrap_or("").to_string();

            let collection = doc.get_str("collection").ok().map(|s| s.to_string());

            let metadata = doc.get_document("metadata").ok().and_then(|m| {
                mongodb::bson::from_bson::<Value>(mongodb::bson::Bson::Document(m.clone())).ok()
            });

            Ok(Some(Document {
                id,
                text,
                collection,
                metadata,
                embedding: None,
            }))
        } else {
            Ok(None)
        }
    }
}
