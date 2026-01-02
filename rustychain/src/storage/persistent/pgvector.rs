//! pgvector

use pgvector::Vector;
use serde_json::Value;
use sqlx::{FromRow, Pool, Postgres, Row};

use crate::{
    CollectionStats, Cursor, DEFAULT_COLLECTION_NAME, DEFAULT_DIMENSIONS, Document, SearchOptions,
    SearchResult, VectorStore,
};

/// PostgreSQL + pgvector implementation of VectorStore
#[derive(Clone)]
pub struct PgVectorStore {
    pub pool: Pool<Postgres>,
    pub table_name: Option<String>,
    pub dimensions: Option<usize>,
}

impl PgVectorStore {
    pub async fn new(
        db_url: &str,
        table_name: Option<String>,
        dimensions: Option<usize>,
    ) -> crate::Result<Self> {
        let pool = Pool::<Postgres>::connect(db_url).await?;
        let store = PgVectorStore {
            pool,
            table_name,
            dimensions,
        };
        store.ensure_table_exists().await?;
        Ok(store)
    }

    fn table(&self) -> &str {
        self.table_name
            .as_deref()
            .unwrap_or(DEFAULT_COLLECTION_NAME)
    }

    /// Ensure the vector table exists with proper schema
    async fn ensure_table_exists(&self) -> crate::Result<()> {
        // CREATE EXTENSION should be idempotent, but in some environments
        // concurrent test runs or older Postgres versions may return a
        // duplicate-key error when the extension already exists. Detect
        // that specific database error (Postgres code 23505) and ignore it.
        match sqlx::query("CREATE EXTENSION IF NOT EXISTS vector")
            .execute(&self.pool)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                let ignore = match &e {
                    sqlx::Error::Database(db_err) => {
                        matches!(db_err.code(), Some(code) if code == "23505")
                    }
                    _ => false,
                };

                if !ignore {
                    return Err(e.into());
                }
            }
        }

        let query = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {} (
                id SERIAL PRIMARY KEY,
                text TEXT NOT NULL,
                embedding vector({}) NOT NULL,
                collection VARCHAR(255),
                metadata JSONB,
                created_at TIMESTAMPTZ DEFAULT NOW()
            )
            "#,
            self.table(),
            self.dims()
        );
        sqlx::query(&query).execute(&self.pool).await?;

        // Index for similarity search
        let embedding_idx = format!(
            r#"
            CREATE INDEX IF NOT EXISTS {}_embedding_idx 
            ON {} 
            USING hnsw (embedding vector_cosine_ops)
            "#,
            self.table(),
            self.table()
        );
        sqlx::query(&embedding_idx).execute(&self.pool).await?;

        // Index for collection filtering
        let collection_idx = format!(
            "CREATE INDEX IF NOT EXISTS {}_collection_idx ON {} (collection)",
            self.table(),
            self.table()
        );
        sqlx::query(&collection_idx).execute(&self.pool).await?;

        // GIN index for metadata JSONB
        let metadata_idx = format!(
            "CREATE INDEX IF NOT EXISTS {}_metadata_idx ON {} USING GIN (metadata)",
            self.table(),
            self.table()
        );
        sqlx::query(&metadata_idx).execute(&self.pool).await?;

        Ok(())
    }

    /// Rebuild the HNSW index for the table
    pub async fn reindex_internal(&self) -> crate::Result<()> {
        let index_name = format!("{}_embedding_idx", self.table());

        // Drop existing index and recreate
        let drop_query = format!("DROP INDEX IF EXISTS {}", index_name);
        sqlx::query(&drop_query).execute(&self.pool).await?;

        let create_query = format!(
            "CREATE INDEX {} ON {} USING hnsw (embedding vector_cosine_ops)",
            index_name,
            self.table()
        );
        sqlx::query(&create_query).execute(&self.pool).await?;

        Ok(())
    }
}

#[async_trait::async_trait]
impl VectorStore for PgVectorStore {
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
        let vector = Vector::from(embedding);
        let query = format!(
            "INSERT INTO {} (text, embedding, collection, metadata) VALUES ($1, $2, $3, $4) RETURNING id",
            self.table()
        );

        let row = sqlx::query(&query)
            .bind(&text)
            .bind(&vector)
            .bind(&collection)
            .bind(&metadata)
            .fetch_one(&self.pool)
            .await?;

        let id: i32 = row.get("id");
        Ok(id.to_string())
    }

    async fn search(
        &self,
        query_embedding: Vec<f32>,
        options: SearchOptions,
    ) -> crate::Result<Vec<SearchResult>> {
        let query_vector = Vector::from(query_embedding);

        let embedding_select = if options.include_embeddings {
            ", embedding"
        } else {
            ""
        };

        let distance_select = if options.include_distances {
            ", embedding <=> $1 as distance"
        } else {
            ""
        };

        // Build WHERE clauses
        let mut where_clauses = Vec::new();
        let mut param_idx = 2; // $1 is query_vector

        if options.collection.is_some() {
            where_clauses.push(format!("collection = ${}", param_idx));
            param_idx += 1;
        }

        if options.threshold.is_some() {
            where_clauses.push(format!("embedding <=> $1 < ${}", param_idx));
            param_idx += 1;
        }

        if let Some(ref filter) = options.metadata_filter {
            // Support simple key-value filters like {"key": "value"}
            for (key, _value) in filter.as_object().unwrap_or(&serde_json::Map::new()) {
                where_clauses.push(format!("metadata->>'{}' = ${}", key, param_idx));
                param_idx += 1;
            }
        }

        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let query = format!(
            r#"
            SELECT id, text, collection, metadata {} {}
            FROM {}
            {}
            ORDER BY embedding <=> $1
            LIMIT ${}
            "#,
            embedding_select,
            distance_select,
            self.table(),
            where_sql,
            param_idx
        );

        let mut q = sqlx::query(&query).bind(&query_vector);

        if let Some(ref collection) = options.collection {
            q = q.bind(collection);
        }

        if let Some(threshold) = options.threshold {
            q = q.bind(threshold);
        }

        if let Some(ref filter) = options.metadata_filter {
            for (_key, value) in filter.as_object().unwrap_or(&serde_json::Map::new()) {
                if let Some(s) = value.as_str() {
                    q = q.bind(s.to_string());
                }
            }
        }

        q = q.bind(options.k as i32);

        let rows = q.fetch_all(&self.pool).await?;

        let results = rows
            .into_iter()
            .map(|row| {
                let distance: f32 = if options.include_distances {
                    row.try_get("distance").unwrap_or(0.0)
                } else {
                    0.0
                };

                let embedding = if options.include_embeddings {
                    row.try_get::<Vector, _>("embedding")
                        .ok()
                        .map(|v| v.to_vec())
                } else {
                    None
                };

                let id: i32 = row.get("id");

                SearchResult {
                    document: Document {
                        id: id.to_string(),
                        text: row.get("text"),
                        collection: row.get("collection"),
                        metadata: row.get("metadata"),
                        embedding,
                    },
                    distance,
                    score: 1.0 - distance,
                }
            })
            .collect();

        Ok(results)
    }

    async fn delete(&self, id: &str) -> crate::Result<bool> {
        let id_num: i32 = id
            .parse()
            .map_err(|_| crate::Error::Input(format!("Invalid ID format: {}", id)))?;

        let query = format!("DELETE FROM {} WHERE id = $1", self.table());
        let result = sqlx::query(&query).bind(id_num).execute(&self.pool).await?;

        Ok(result.rows_affected() > 0)
    }

    async fn delete_collection(&self, collection: &str) -> crate::Result<u64> {
        let query = format!("DELETE FROM {} WHERE collection = $1", self.table());
        let result = sqlx::query(&query)
            .bind(collection)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }

    async fn clear(&self) -> crate::Result<()> {
        let query = format!("TRUNCATE TABLE {} RESTART IDENTITY", self.table());
        sqlx::query(&query).execute(&self.pool).await?;
        Ok(())
    }

    async fn count(&self, collection: Option<&str>) -> crate::Result<i64> {
        let query = match collection {
            Some(_) => format!(
                "SELECT COUNT(*) as count FROM {} WHERE collection = $1",
                self.table()
            ),
            None => format!("SELECT COUNT(*) as count FROM {}", self.table()),
        };

        let row = match collection {
            Some(c) => sqlx::query(&query).bind(c).fetch_one(&self.pool).await?,
            None => sqlx::query(&query).fetch_one(&self.pool).await?,
        };

        Ok(row.get("count"))
    }

    async fn list_collections(&self) -> crate::Result<Vec<String>> {
        let query = format!(
            "SELECT DISTINCT collection FROM {} WHERE collection IS NOT NULL",
            self.table()
        );

        let rows = sqlx::query(&query).fetch_all(&self.pool).await?;

        let collections = rows
            .into_iter()
            .filter_map(|row| row.try_get::<String, _>("collection").ok())
            .collect();

        Ok(collections)
    }

    async fn reindex(&self) -> crate::Result<()> {
        self.reindex_internal().await
    }

    async fn stats(&self, collection: Option<&str>) -> crate::Result<CollectionStats> {
        let query = format!(
            r#"
            SELECT 
                pg_total_relation_size('{}') as total_size,
                pg_indexes_size('{}') as index_size,
                (SELECT count(*) FROM {}{}) as row_count
            "#,
            self.table(),
            self.table(),
            self.table(),
            if collection.is_some() {
                " WHERE collection = $1"
            } else {
                ""
            }
        );

        let row = if let Some(coll) = collection {
            sqlx::query(&query).bind(coll).fetch_one(&self.pool).await?
        } else {
            sqlx::query(&query).fetch_one(&self.pool).await?
        };

        let total_size: i64 = row.try_get("total_size").unwrap_or(0);
        let index_size: i64 = row.try_get("index_size").unwrap_or(0);
        let row_count: i64 = row.try_get("row_count").unwrap_or(0);

        Ok(CollectionStats {
            total_documents: row_count,
            total_size_bytes: Some(total_size),
            metadata: Some(serde_json::json!({
                "index_size_bytes": index_size,
            })),
        })
    }

    async fn delete_by_metadata(
        &self,
        filter: &Value,
        collection: Option<&str>,
    ) -> crate::Result<u64> {
        let query = match collection {
            Some(_) => format!(
                "DELETE FROM {} WHERE metadata @> $1 AND collection = $2",
                self.table()
            ),
            None => format!("DELETE FROM {} WHERE metadata @> $1", self.table()),
        };

        let result = match collection {
            Some(coll) => {
                sqlx::query(&query)
                    .bind(filter)
                    .bind(coll)
                    .execute(&self.pool)
                    .await?
            }
            None => sqlx::query(&query).bind(filter).execute(&self.pool).await?,
        };

        Ok(result.rows_affected())
    }

    async fn list(
        &self,
        limit: usize,
        cursor: Option<Cursor>,
        collection: Option<&str>,
    ) -> crate::Result<(Vec<Document>, Option<Cursor>)> {
        let offset = cursor.as_ref().and_then(|c| c.offset).unwrap_or(0);

        let query = match collection {
            Some(_) => format!(
                "SELECT id, text, collection, metadata FROM {} WHERE collection = $1 ORDER BY id LIMIT $2 OFFSET $3",
                self.table()
            ),
            None => format!(
                "SELECT id, text, collection, metadata FROM {} ORDER BY id LIMIT $1 OFFSET $2",
                self.table()
            ),
        };

        #[derive(FromRow)]
        struct PgDocument {
            id: i32,
            text: String,
            collection: Option<String>,
            metadata: Option<Value>,
        }

        let rows: Vec<PgDocument> = match collection {
            Some(coll) => {
                sqlx::query_as(&query)
                    .bind(coll)
                    .bind(limit as i32)
                    .bind(offset)
                    .fetch_all(&self.pool)
                    .await?
            }
            None => {
                sqlx::query_as(&query)
                    .bind(limit as i32)
                    .bind(offset)
                    .fetch_all(&self.pool)
                    .await?
            }
        };

        let has_more = rows.len() == limit;
        let next_cursor = if has_more {
            Some(Cursor {
                offset: Some(offset + limit as i64),
                last_id: None,
            })
        } else {
            None
        };

        let documents = rows
            .into_iter()
            .map(|row| Document {
                id: row.id.to_string(),
                text: row.text,
                collection: row.collection,
                metadata: row.metadata,
                embedding: None,
            })
            .collect();

        Ok((documents, next_cursor))
    }

    async fn get(&self, id: &str) -> crate::Result<Option<Document>> {
        let id_num: i32 = id
            .parse()
            .map_err(|_| crate::Error::Input(format!("Invalid ID format: {}", id)))?;

        let query = format!(
            "SELECT id, text, collection, metadata FROM {} WHERE id = $1",
            self.table()
        );

        #[derive(FromRow)]
        struct PgDocument {
            id: i32,
            text: String,
            collection: Option<String>,
            metadata: Option<Value>,
        }

        let row: Option<PgDocument> = sqlx::query_as(&query)
            .bind(id_num)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(|r| Document {
            id: r.id.to_string(),
            text: r.text,
            collection: r.collection,
            metadata: r.metadata,
            embedding: None,
        }))
    }
}
