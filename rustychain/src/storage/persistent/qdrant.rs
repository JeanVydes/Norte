//! qdrant

use crate::{
    CollectionStats, Cursor, DEFAULT_COLLECTION_NAME, DEFAULT_DIMENSIONS, Document, SearchOptions,
    SearchResult, VectorStore,
};

use qdrant_client::qdrant::{
    Condition, CreateCollectionBuilder, Distance, Filter, PointId, PointStruct,
    ScrollPointsBuilder, SearchPointsBuilder, VectorParamsBuilder,
};
use qdrant_client::{Payload, Qdrant};
use serde_json::Value;

/// Qdrant implementation of VectorStore
#[derive(Clone)]
pub struct QdrantStore {
    pub client: Qdrant,
    pub collection_name: Option<String>,
    pub dimensions: Option<usize>,
}

impl QdrantStore {
    pub async fn new(
        url: &str,
        collection_name: Option<String>,
        dimensions: Option<usize>,
    ) -> crate::Result<Self> {
        let client = Qdrant::from_url(url).build()?;
        let store = QdrantStore {
            client,
            collection_name,
            dimensions,
        };
        store.ensure_collection_exists().await?;
        Ok(store)
    }

    pub async fn with_api_key(
        url: &str,
        api_key: impl Into<String>,
        collection_name: Option<String>,
        dimensions: Option<usize>,
    ) -> crate::Result<Self> {
        let client = Qdrant::from_url(url).api_key(api_key.into()).build()?;
        let store = QdrantStore {
            client,
            collection_name,
            dimensions,
        };
        store.ensure_collection_exists().await?;
        Ok(store)
    }

    fn collection(&self) -> &str {
        self.collection_name
            .as_deref()
            .unwrap_or(DEFAULT_COLLECTION_NAME)
    }

    /// Ensure the collection exists with proper configuration
    async fn ensure_collection_exists(&self) -> crate::Result<()> {
        let collection_name = self.collection();

        // Check if collection exists
        let exists = self
            .client
            .collection_exists(collection_name)
            .await
            .unwrap_or(false);

        if !exists {
            self.client
                .create_collection(
                    CreateCollectionBuilder::new(collection_name).vectors_config(
                        VectorParamsBuilder::new(self.dims() as u64, Distance::Cosine),
                    ),
                )
                .await?;
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl VectorStore for QdrantStore {
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
        let id = uuid::Uuid::new_v4().to_string();

        let mut payload = Payload::new();
        payload.insert("text", text);

        if let Some(coll) = collection {
            payload.insert("collection", coll);
        }

        if let Some(meta) = metadata {
            payload.insert("metadata", meta);
        }

        let point = PointStruct::new(id.clone(), embedding, payload);

        let _ = self
            .client
            .upsert_points(qdrant_client::qdrant::UpsertPointsBuilder::new(
                self.collection(),
                vec![point],
            ))
            .await?;

        Ok(id)
    }

    async fn search(
        &self,
        query_embedding: Vec<f32>,
        options: SearchOptions,
    ) -> crate::Result<Vec<SearchResult>> {
        let mut filter_conditions = Vec::new();

        // Add collection filter
        if let Some(ref coll) = options.collection {
            filter_conditions.push(Condition::matches("collection", coll.clone()));
        }

        // Add metadata filters
        if let Some(ref metadata_filter) = options.metadata_filter
            && let Some(obj) = metadata_filter.as_object()
        {
            for (key, value) in obj {
                let field_name = format!("metadata.{}", key);
                if let Some(v) = value.as_str() {
                    filter_conditions.push(Condition::matches(field_name, v.to_string()));
                } else if let Some(v) = value.as_i64() {
                    filter_conditions.push(Condition::matches(field_name, v));
                } else if let Some(v) = value.as_bool() {
                    filter_conditions.push(Condition::matches(field_name, v));
                }
            }
        }

        let mut search_builder =
            SearchPointsBuilder::new(self.collection(), query_embedding, options.k as u64)
                .with_payload(true)
                .with_vectors(options.include_embeddings);

        if !filter_conditions.is_empty() {
            search_builder = search_builder.filter(Filter::must(filter_conditions));
        }

        if let Some(threshold) = options.threshold {
            search_builder = search_builder.score_threshold(threshold);
        }

        let search_result = self.client.search_points(search_builder).await?;

        let results = search_result
            .result
            .into_iter()
            .map(|point| {
                let id = point
                    .id
                    .map(|id| match id.point_id_options {
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => {
                            n.to_string()
                        }
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                        None => String::new(),
                    })
                    .unwrap_or_default();

                let text = point
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let collection = point
                    .payload
                    .get("collection")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let metadata = point
                    .payload
                    .get("metadata")
                    .and_then(|v| serde_json::to_value(v).ok());

                let embedding = if options.include_embeddings {
                    point.vectors.and_then(|v| v.get_vector())
                } else {
                    None
                };

                let distance = if options.include_distances {
                    1.0 - point.score
                } else {
                    0.0
                };

                #[allow(clippy::manual_map)]
                let embedding: Option<Vec<f32>> = match embedding {
                    Some(v) => Some(match v {
                        qdrant_client::qdrant::vector_output::Vector::Dense(vec) => vec.data,
                        qdrant_client::qdrant::vector_output::Vector::Sparse(vec) => vec.values,
                        qdrant_client::qdrant::vector_output::Vector::MultiDense(vecs) => {
                            vecs.vectors.into_iter().flat_map(|v| v.data).collect()
                        }
                    }),
                    _ => None,
                };

                SearchResult {
                    document: Document {
                        id,
                        text,
                        collection,
                        metadata,
                        embedding,
                    },
                    distance,
                    score: point.score,
                }
            })
            .collect();

        Ok(results)
    }

    async fn delete(&self, id: &str) -> crate::Result<bool> {
        // Try parsing as UUID first, then as numeric ID
        let point_id: qdrant_client::qdrant::PointId = if let Ok(uuid) = uuid::Uuid::parse_str(id) {
            uuid.to_string().into()
        } else if let Ok(num) = id.parse::<u64>() {
            num.into()
        } else {
            return Ok(false);
        };

        self.client
            .delete_points(
                qdrant_client::qdrant::DeletePointsBuilder::new(self.collection())
                    .points(vec![point_id]),
            )
            .await?;

        Ok(true)
    }

    async fn delete_collection(&self, collection: &str) -> crate::Result<u64> {
        let filter = Filter::must(vec![Condition::matches(
            "collection",
            collection.to_string(),
        )]);

        let count_before = self.count(Some(collection)).await?;

        self.client
            .delete_points(
                qdrant_client::qdrant::DeletePointsBuilder::new(self.collection()).points(filter),
            )
            .await?;

        Ok(count_before as u64)
    }

    async fn clear(&self) -> crate::Result<()> {
        self.client.delete_collection(self.collection()).await?;
        self.ensure_collection_exists().await?;
        Ok(())
    }

    async fn count(&self, collection: Option<&str>) -> crate::Result<i64> {
        if let Some(coll) = collection {
            // For filtered count, we need to use scroll to get approximate count
            let filter = Filter::must(vec![Condition::matches("collection", coll.to_string())]);

            let scroll_result = self
                .client
                .scroll(
                    ScrollPointsBuilder::new(self.collection())
                        .filter(filter)
                        .limit(1),
                )
                .await?;

            // This is a rough estimate - for exact count would need to scroll all pages
            Ok(scroll_result.result.len() as i64)
        } else {
            let info = self.client.collection_info(self.collection()).await?;
            let collection_info = info.result.ok_or_else(|| {
                crate::Error::NotFound("Collection info not available".to_string())
            })?;
            Ok(collection_info.points_count.unwrap_or(0) as i64)
        }
    }

    async fn list_collections(&self) -> crate::Result<Vec<String>> {
        // Scroll through all points to find unique collections
        let mut collections = std::collections::HashSet::new();
        let mut next_offset = None;

        loop {
            let mut scroll_builder = ScrollPointsBuilder::new(self.collection())
                .limit(100)
                .with_payload(true); // Use boolean instead of array

            if let Some(offset) = next_offset {
                scroll_builder = scroll_builder.offset(offset);
            }

            let scroll_result = self.client.scroll(scroll_builder).await?;

            for point in &scroll_result.result {
                if let Some(coll_value) = point.payload.get("collection")
                    && let Some(coll_str) = coll_value.as_str()
                {
                    collections.insert(coll_str.to_string());
                }
            }

            if scroll_result.next_page_offset.is_none() {
                break;
            }
            next_offset = scroll_result.next_page_offset;
        }

        Ok(collections.into_iter().collect())
    }

    async fn stats(&self, collection: Option<&str>) -> crate::Result<CollectionStats> {
        let info = self.client.collection_info(self.collection()).await?;
        let collection_info = info
            .result
            .ok_or_else(|| crate::Error::NotFound("Collection info not available".to_string()))?;

        let total_docs = if let Some(coll) = collection {
            self.count(Some(coll)).await?
        } else {
            collection_info.points_count.unwrap_or(0) as i64
        };

        Ok(CollectionStats {
            total_documents: total_docs,
            total_size_bytes: None, // Qdrant doesn't expose byte size
            metadata: Some(serde_json::json!({
                "segments_count": collection_info.segments_count,
                "vectors_count": collection_info.segments_count,
                "indexed_vectors_count": collection_info.indexed_vectors_count.unwrap_or(0),
                "indexing_progress": format!(
                    "{:.1}%",
                    if collection_info.segments_count == 0 {
                        100.0
                    } else {
                        (collection_info.indexed_vectors_count.unwrap_or(0) as f64
                            / collection_info.segments_count as f64)
                            * 100.0
                    }
                ),
            })),
        })
    }

    async fn delete_by_metadata(
        &self,
        filter: &Value,
        collection: Option<&str>,
    ) -> crate::Result<u64> {
        let mut conditions = Vec::new();

        if let Some(coll) = collection {
            conditions.push(Condition::matches("collection", coll.to_string()));
        }

        if let Some(obj) = filter.as_object() {
            for (key, value) in obj {
                let field_name = format!("metadata.{}", key);
                if let Some(v) = value.as_str() {
                    conditions.push(Condition::matches(field_name, v.to_string()));
                } else if let Some(v) = value.as_i64() {
                    conditions.push(Condition::matches(field_name, v));
                } else if let Some(v) = value.as_bool() {
                    conditions.push(Condition::matches(field_name, v));
                }
            }
        }

        let filter = Filter::must(conditions);

        // Count before deletion (approximate)
        let count = self.count(collection).await?;

        self.client
            .delete_points(
                qdrant_client::qdrant::DeletePointsBuilder::new(self.collection()).points(filter),
            )
            .await?;

        Ok(count as u64)
    }

    async fn list(
        &self,
        limit: usize,
        cursor: Option<Cursor>,
        collection: Option<&str>,
    ) -> crate::Result<(Vec<Document>, Option<Cursor>)> {
        let mut scroll_builder = ScrollPointsBuilder::new(self.collection())
            .limit(limit as u32)
            .with_payload(true);

        if let Some(coll) = collection {
            scroll_builder = scroll_builder.filter(Filter::must(vec![Condition::matches(
                "collection",
                coll.to_string(),
            )]));
        }

        if let Some(c) = cursor
            && let Some(last_id) = c.last_id
        {
            // Try parsing as UUID first, then as numeric
            if let Ok(uuid) = uuid::Uuid::parse_str(&last_id) {
                scroll_builder = scroll_builder.offset::<PointId>(uuid.to_string().into());
            } else if let Ok(num) = last_id.parse::<u64>() {
                scroll_builder = scroll_builder.offset::<PointId>(num.into());
            }
        }

        let scroll_result = self.client.scroll(scroll_builder).await?;

        let has_more = scroll_result.next_page_offset.is_some();
        let next_cursor = if has_more {
            scroll_result.next_page_offset.map(|offset| {
                let id_str = match offset.point_id_options {
                    Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
                    Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                    None => String::new(),
                };
                Cursor {
                    offset: None,
                    last_id: Some(id_str),
                }
            })
        } else {
            None
        };

        let documents = scroll_result
            .result
            .into_iter()
            .map(|point| {
                let id = point
                    .id
                    .map(|id| match id.point_id_options {
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => {
                            n.to_string()
                        }
                        Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u,
                        None => String::new(),
                    })
                    .unwrap_or_default();

                let text = point
                    .payload
                    .get("text")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();

                let collection = point
                    .payload
                    .get("collection")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let metadata = point
                    .payload
                    .get("metadata")
                    .and_then(|v| serde_json::to_value(v).ok());

                Document {
                    id,
                    text,
                    collection,
                    metadata,
                    embedding: None,
                }
            })
            .collect();

        Ok((documents, next_cursor))
    }

    async fn get(&self, id: &str) -> crate::Result<Option<Document>> {
        // Try parsing as UUID first, then as numeric ID
        let point_id = if let Ok(uuid) = uuid::Uuid::parse_str(id) {
            vec![uuid.to_string().into()]
        } else if let Ok(num) = id.parse::<u64>() {
            vec![num.into()]
        } else {
            return Ok(None);
        };

        let points = self
            .client
            .get_points(
                qdrant_client::qdrant::GetPointsBuilder::new(self.collection(), point_id)
                    .with_payload(true),
            )
            .await?;

        if let Some(point) = points.result.first() {
            let id = point
                .id
                .as_ref()
                .map(|id| match &id.point_id_options {
                    Some(qdrant_client::qdrant::point_id::PointIdOptions::Num(n)) => n.to_string(),
                    Some(qdrant_client::qdrant::point_id::PointIdOptions::Uuid(u)) => u.clone(),
                    None => String::new(),
                })
                .unwrap_or_default();

            let text = point
                .payload
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_default();

            let collection = point
                .payload
                .get("collection")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let metadata = point
                .payload
                .get("metadata")
                .and_then(|v| serde_json::to_value(v).ok());

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
