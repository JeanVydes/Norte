//! Common test utilities and fixtures
//!
//! This module provides shared test infrastructure including:
//! - PostgreSQL container setup with pgvector
//! - Mock embeddings generation
//! - Test fixtures

use testcontainers::{
    GenericImage, ImageExt,
    core::{IntoContainerPort, WaitFor},
    runners::AsyncRunner,
};
use tokio::sync::OnceCell;
/// Default embedding dimensions for tests
pub const TEST_DIMENSIONS: i32 = 384;

/// Database connection for tests
static DB_CONNECTION: OnceCell<String> = OnceCell::const_new();

/// Get database connection string for tests
///
/// Prefers DATABASE_URL env var, falls back to testcontainers
pub async fn get_test_db_url() -> &'static str {
    DB_CONNECTION
        .get_or_init(|| async {
            // First, check if DATABASE_URL is set (for using existing DB)
            if let Ok(url) = std::env::var("DATABASE_URL") {
                return url;
            }

            // Check if default local postgres is available
            let default_url = "postgresql://test:test@127.0.0.1:5432/test";
            if sqlx::PgPool::connect(default_url).await.is_ok() {
                return default_url.to_string();
            }

            // Fall back to testcontainers
            let container = GenericImage::new("pgvector/pgvector", "pg16")
                .with_exposed_port(5432.tcp())
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_env_var("POSTGRES_USER", "test")
                .with_env_var("POSTGRES_PASSWORD", "test")
                .with_env_var("POSTGRES_DB", "test")
                .start()
                .await
                .expect("Failed to start PostgreSQL container");

            let host_port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("Failed to get host port");

            // Keep container alive by leaking it (tests are short-lived)
            std::mem::forget(container);

            // Wait for postgres to be ready
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;

            format!("postgresql://test:test@127.0.0.1:{}/test", host_port)
        })
        .await
}

/// Generate a random embedding vector of specified dimensions
pub fn random_embedding(dims: i32) -> Vec<f32> {
    use rand::Rng;
    let mut rng = rand::rng();
    (0..dims).map(|_| rng.random_range(-1.0..1.0)).collect()
}

/// Generate a normalized random embedding (unit vector)
pub fn normalized_embedding(dims: i32) -> Vec<f32> {
    let embedding = random_embedding(dims);
    let magnitude: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
    embedding.into_iter().map(|x| x / magnitude).collect()
}

/// Generate similar embeddings (for testing similarity search)
pub fn similar_embeddings(base: &[f32], count: usize, variance: f32) -> Vec<Vec<f32>> {
    use rand::Rng;
    let mut rng = rand::rng();

    (0..count)
        .map(|_| {
            base.iter()
                .map(|&x| x + rng.random_range(-variance..variance))
                .collect()
        })
        .collect()
}

/// Create a unique table name for test isolation
pub fn unique_table_name() -> String {
    use rand::Rng;
    let suffix: u32 = rand::rng().random_range(1000..9999);
    format!("test_documents_{}", suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_embedding_dimensions() {
        let embedding = random_embedding(384);
        assert_eq!(embedding.len(), 384);
    }

    #[test]
    fn test_normalized_embedding_is_unit_vector() {
        let embedding = normalized_embedding(384);
        let magnitude: f32 = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((magnitude - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_similar_embeddings() {
        let base = normalized_embedding(384);
        let similar = similar_embeddings(&base, 5, 0.1);
        assert_eq!(similar.len(), 5);

        // Each should be close to base
        for emb in &similar {
            let diff: f32 = base
                .iter()
                .zip(emb.iter())
                .map(|(a, b)| (a - b).abs())
                .sum();
            assert!(diff < 384.0 * 0.2); // Average diff < 0.2 per dimension
        }
    }
}
