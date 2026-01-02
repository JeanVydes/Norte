//! kv_lru
//!
//! A Key-Value storage tool using an LRU cache policy with configurable capacity.

use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use rustychain::memory::lru::LRUCache;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KvPair {
    pub key: String,
    pub value: serde_json::Value,
}

impl Default for KvPair {
    fn default() -> Self {
        Self {
            key: String::new(),
            value: serde_json::json!(null),
        }
    }
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct KvArgs {
    #[schemars(description = "Action to perform: 'set', 'get', 'list', or 'clear'.")]
    pub action: String,
    #[schemars(description = "The key to store or retrieve.")]
    pub key: Option<String>,
    #[schemars(description = "The value to store (required for 'set').")]
    pub value: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct KvLruTool<const N: usize> {
    pub cache: Arc<Mutex<LRUCache<KvPair, N>>>,
}

impl<const N: usize> KvLruTool<N> {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(Mutex::new(LRUCache::new())),
        }
    }
}

impl<const N: usize> Default for KvLruTool<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl<const N: usize> FnExecutor<KvArgs, serde_json::Value> for KvLruTool<N> {
    async fn call(&self, args: KvArgs) -> rustychain::Result<serde_json::Value> {
        let mut cache = self.cache.lock().await;

        match args.action.to_lowercase().as_str() {
            "set" => {
                let key = args
                    .key
                    .ok_or_else(|| rustychain::Error::Internal("Key required".into()))?;
                let value = args
                    .value
                    .ok_or_else(|| rustychain::Error::Internal("Value required".into()))?;

                let updated = if cache.touch(|item| item.key == key) {
                    if let Some(entry) = cache.front_mut() {
                        entry.value = value;
                    }
                    true
                } else {
                    cache.insert(KvPair { key, value });
                    false
                };

                Ok(serde_json::json!({
                    "status": "success",
                    "updated": updated,
                    "current_len": cache.len(),
                    "capacity": N
                }))
            }
            "get" => {
                let key = args
                    .key
                    .ok_or_else(|| rustychain::Error::Internal("Key required".into()))?;
                if let Some(pair) = cache.find(|item| item.key == key) {
                    Ok(serde_json::json!({ "key": pair.key, "value": pair.value }))
                } else {
                    Ok(serde_json::json!({ "error": "key not found" }))
                }
            }
            "list" => {
                let items: Vec<KvPair> = cache.iter().cloned().collect();
                Ok(serde_json::json!({ "items": items, "capacity": N }))
            }
            "clear" => {
                cache.clear();
                Ok(serde_json::json!({ "status": "cache cleared" }))
            }
            _ => Err(rustychain::Error::Internal("Invalid action".into())),
        }
    }
}

impl<const N: usize> FnDeclarator<KvArgs, serde_json::Value> for KvLruTool<N> {
    fn declare(&self) -> FunctionDeclaration<KvArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "kv_lru_tool",
            description: "Short-term memory with LRU eviction policy. Use this to store temporary state, task progress, or variables.",
            parameters: schema_for!(KvArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
