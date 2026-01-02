use serde::{Deserialize, Serialize};

/// Maximum number of search results allowed per query
pub const MAX_RESULTS_LIMIT: usize = 100;
/// Minimum number of search results
pub const MIN_RESULTS_LIMIT: usize = 1;
/// Default maximum number of results
pub const DEFAULT_MAX_RESULTS: usize = 10;
/// Default region code
pub const DEFAULT_REGION: &str = "wt-wt";
/// HTTP request timeout in seconds
pub const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Cache capacity (number of queries to cache)
pub const CACHE_CAPACITY: usize = 100;
/// Minimum delay between requests in milliseconds
pub const RATE_LIMIT_DELAY_MS: u64 = 500;

pub fn default_max_results() -> usize {
    DEFAULT_MAX_RESULTS
}

pub fn default_region() -> String {
    DEFAULT_REGION.to_string()
}

pub fn default_use_cache() -> bool {
    true
}

#[async_trait::async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        region: &str,
    ) -> rustychain::Result<Vec<SearchResult>>;

    fn name(&self) -> &'static str;
}

/// A single search result from DuckDuckGo
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SearchResult {
    /// The title of the search result
    pub title: String,
    /// The URL of the result
    pub url: String,
    /// Optional snippet/description of the result
    pub snippet: Option<String>,
}
