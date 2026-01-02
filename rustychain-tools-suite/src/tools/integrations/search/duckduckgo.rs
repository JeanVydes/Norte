//!! DuckDuckGo Search Tool
//!
//! This module provides a robust tool for performing web searches using DuckDuckGo.
//!
//! # Features
//!
//! - Configurable search parameters (query, max results, region)
//! - Optional LRU caching to avoid redundant network requests
//! - Rate limiting to prevent IP bans
//! - URL validation and sanitization
//! - Rotating user agents to avoid detection
//! - Timeout handling and error recovery
use crate::tools::integrations::search::SearchProvider;
use crate::tools::integrations::search::definitions::{
    CACHE_CAPACITY, MAX_RESULTS_LIMIT, MIN_RESULTS_LIMIT, RATE_LIMIT_DELAY_MS,
    REQUEST_TIMEOUT_SECS, SearchResult, default_max_results, default_region, default_use_cache,
};

use async_trait::async_trait;
use reqwest::Client;
use rustychain::{
    FnDeclarator, FunctionDeclaration,
    llm::function::FnExecutor,
    memory::lru::LRUCache,
    util::{UrlValidator, UserAgentFactory},
};
use schemars::JsonSchema;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Arguments for performing a DuckDuckGo search
#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct DuckDuckGoSearchArgs {
    #[schemars(description = "The search query string (1-500 characters).")]
    pub query: String,

    #[schemars(description = "Maximum number of results to return (1-100).")]
    #[serde(default = "default_max_results")]
    pub max_results: usize,

    #[schemars(
        description = "Region code for localized search results (e.g., 'wt-wt' for worldwide, 'us-en' for USA)."
    )]
    #[serde(default = "default_region")]
    pub region: String,

    #[schemars(description = "Whether to use cached results if available.")]
    #[serde(default = "default_use_cache")]
    pub use_cache: bool,
}

impl DuckDuckGoSearchArgs {
    /// Validates the search arguments
    ///
    /// Returns an error if any argument is invalid
    pub fn validate(&self) -> rustychain::Result<()> {
        // Validate query
        let trimmed_query = self.query.trim();
        if trimmed_query.is_empty() {
            return Err(rustychain::Error::Search {
                query: None,
                source: None,
                feedback: Some("Search query cannot be empty".to_string()),
            });
        }
        if trimmed_query.len() > 500 {
            return Err(rustychain::Error::Search {
                query: None,
                source: None,
                feedback: Some("Search query cannot exceed 500 characters".to_string()),
            });
        }

        // Validate max_results
        if self.max_results < MIN_RESULTS_LIMIT {
            return Err(rustychain::Error::Search {
                query: None,
                source: None,
                feedback: Some(format!(
                    "max_results must be at least {}",
                    MIN_RESULTS_LIMIT
                )),
            });
        }
        if self.max_results > MAX_RESULTS_LIMIT {
            return Err(rustychain::Error::Search {
                query: None,
                source: None,
                feedback: Some(format!("max_results cannot exceed {}", MAX_RESULTS_LIMIT)),
            });
        }

        // Validate region code format (basic check)
        if self.region.is_empty() || self.region.len() > 10 {
            return Err(rustychain::Error::Search {
                query: None,
                source: None,
                feedback: Some("Region code must be 1-10 characters".to_string()),
            });
        }

        Ok(())
    }
}

/// Response from a DuckDuckGo search
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuckDuckGoSearchResponse {
    /// The search results
    pub results: Vec<SearchResult>,
    /// The original query
    pub query: String,
    /// Number of results returned
    pub count: usize,
    /// Whether results were served from cache
    pub from_cache: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct DuckDuckGoSearchCached {
    pub params: DuckDuckGoSearchArgs,
    pub results: Vec<SearchResult>,
}

#[derive(Clone)]
/// Tool for performing DuckDuckGo web searches
pub struct DuckDuckGoSearchTool {
    /// HTTP client for making requests
    client: Client,
    /// LRU cache for search results
    cache: Arc<Mutex<LRUCache<DuckDuckGoSearchCached, CACHE_CAPACITY>>>,
    /// Last request timestamp for rate limiting
    last_request: Arc<Mutex<Option<tokio::time::Instant>>>,
}

impl Default for DuckDuckGoSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl DuckDuckGoSearchTool {
    /// Creates a new DuckDuckGo search tool with default configuration
    pub fn new() -> Self {
        let client = Client::builder()
            .user_agent(UserAgentFactory::random())
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            cache: Arc::new(Mutex::new(LRUCache::new())),
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// Performs a search with the given arguments
    pub async fn search(
        &self,
        args: DuckDuckGoSearchArgs,
    ) -> rustychain::Result<DuckDuckGoSearchResponse> {
        // Validate arguments
        args.validate()?;

        let pred = |key: &DuckDuckGoSearchCached| -> Option<Vec<SearchResult>> {
            if args == key.params && !key.results.is_empty() {
                return Some(key.results.clone());
            }

            if key.params.query.contains(&args.query)
                && key.params.max_results >= args.max_results
                && key.params.region == args.region
                && !key.results.is_empty()
            {
                let limited_results: Vec<_> =
                    key.results.iter().take(args.max_results).cloned().collect();
                return Some(limited_results);
            }

            None
        };

        // Check cache if enabled
        if args.use_cache {
            let cache = self.cache.lock().await;
            let results = cache
                .lookup_all(pred)
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();

            if !results.is_empty() {
                log::trace!(
                    "Returning {} cached results for query: {}",
                    results.len(),
                    args.query
                );
                let count = results.len();
                return Ok(DuckDuckGoSearchResponse {
                    results,
                    query: args.query,
                    count,
                    from_cache: true,
                });
            }
        }

        // Rate limiting
        self.apply_rate_limit().await;

        // Build search URL
        let url = format!(
            "https://html.duckduckgo.com/html/?q={}&kl={}",
            urlencoding::encode(&args.query),
            urlencoding::encode(&args.region)
        );

        log::trace!(
            "Searching DuckDuckGo: query='{}', max_results={}",
            args.query,
            args.max_results
        );

        // Fetch results
        let response =
            self.client
                .get(&url)
                .send()
                .await
                .map_err(|e| rustychain::Error::Search {
                    query: Some(args.query.clone()),
                    source: Some(Box::new(e)),
                    feedback: Some("Failed to send request to DuckDuckGo".to_string()),
                })?;

        // Check for CAPTCHA or rate limiting
        let status = response.status();
        if status.as_u16() == 429 || status.as_u16() == 503 {
            return Err(rustychain::Error::Search {
                query: Some(args.query),
                source: None,
                feedback: Some("Rate limited or CAPTCHA encountered from DuckDuckGo".to_string()),
            });
        }

        if !status.is_success() {
            return Err(rustychain::Error::Search {
                query: Some(args.query),
                source: None,
                feedback: Some(format!("DuckDuckGo returned error status: {}", status)),
            });
        }

        let html = response
            .text()
            .await
            .map_err(|e| rustychain::Error::Search {
                query: Some(args.query.clone()),
                source: Some(Box::new(e)),
                feedback: Some("Failed to read response text".to_string()),
            })?;

        let results = self.parse_results(&html, args.max_results)?;

        if args.use_cache && !results.is_empty() {
            let mut cache = self.cache.lock().await;
            cache.insert(DuckDuckGoSearchCached {
                params: args.clone(),
                results: results.clone(),
            });
        }

        Ok(DuckDuckGoSearchResponse {
            count: results.len(),
            query: args.query,
            results,
            from_cache: false,
        })
    }

    /// Applies rate limiting by waiting if necessary
    async fn apply_rate_limit(&self) {
        let mut last_request = self.last_request.lock().await;

        if let Some(last) = *last_request {
            let elapsed = last.elapsed();
            let required_delay = Duration::from_millis(RATE_LIMIT_DELAY_MS);

            if elapsed < required_delay {
                let sleep_duration = required_delay - elapsed;
                log::trace!("Rate limiting: sleeping for {:?}", sleep_duration);
                tokio::time::sleep(sleep_duration).await;
            }
        }

        *last_request = Some(tokio::time::Instant::now());
    }

    /// Parses HTML response and extracts search results
    fn parse_results(
        &self,
        html: &str,
        max_results: usize,
    ) -> rustychain::Result<Vec<SearchResult>> {
        let document = Html::parse_document(html);

        let selectors = vec![
            "div.result",
            "div.results_links",
            "div.web-result",
            "article",
        ];

        let mut result_selector = None;
        for selector_str in &selectors {
            if let Ok(selector) = Selector::parse(selector_str)
                && document.select(&selector).next().is_some()
            {
                result_selector = Some(selector);
                log::trace!("Using selector: {}", selector_str);
                break;
            }
        }

        let result_selector = result_selector.ok_or_else(|| rustychain::Error::Search {
            query: None,
            source: None,
            feedback: Some("Could not find any result containers in HTML".to_string()),
        })?;

        let mut results = Vec::new();
        let mut seen_urls = std::collections::HashSet::new();

        for result_div in document.select(&result_selector) {
            if results.len() >= max_results {
                break;
            }

            // Try to extract result from this div
            if let Some(search_result) = self.extract_result(result_div) {
                // Deduplicate by URL
                if seen_urls.insert(search_result.url.clone()) {
                    results.push(search_result);
                }
            }
        }

        log::trace!("Parsed {} unique results from DuckDuckGo", results.len());

        if results.is_empty() {
            log::trace!(
                "No results parsed. HTML preview: {}",
                &html[..html.len().min(500)]
            );
        }

        Ok(results)
    }

    /// Extracts a single search result from a result div
    fn extract_result(&self, result_div: ElementRef) -> Option<SearchResult> {
        // Intentar múltiples selectores para el título
        let title_selectors = vec!["a.result__a", "a.result__url", "h2 a", "a.result-link"];

        let mut title_link = None;
        for selector_str in &title_selectors {
            if let Ok(selector) = Selector::parse(selector_str)
                && let Some(link) = result_div.select(&selector).next()
            {
                title_link = Some(link);
                break;
            }
        }

        let title_link = title_link?;

        // Extract URL
        let href = title_link.value().attr("href")?;
        let extracted_url = self.extract_url(href)?;

        // Validate and sanitize URL
        let validated_url = UrlValidator::validate(&extracted_url)?;
        let sanitized_url = UrlValidator::sanitize(&validated_url)?;

        // Extract title
        let title: String = title_link.text().collect::<Vec<_>>().join(" ");
        let title = title.trim();

        if title.is_empty() {
            log::trace!("Skipping result with empty title");
            return None;
        }

        // Extract snippet
        let snippet_selectors = vec![
            "a.result__snippet",
            "div.result__snippet",
            "div.snippet",
            "p.result-snippet",
        ];

        let mut snippet = None;
        for selector_str in &snippet_selectors {
            if let Ok(selector) = Selector::parse(selector_str)
                && let Some(el) = result_div.select(&selector).next()
            {
                let text: String = el.text().collect::<Vec<_>>().join(" ");
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    snippet = Some(trimmed.to_string());
                    break;
                }
            }
        }

        log::trace!(
            "Extracted result: title='{}', url='{}'",
            title,
            sanitized_url
        );

        Some(SearchResult {
            title: title.to_string(),
            url: sanitized_url,
            snippet,
        })
    }

    /// Extracts the actual URL from DuckDuckGo's redirect link
    fn extract_url(&self, href: &str) -> Option<String> {
        if let Some(start) = href.find("uddg=") {
            let encoded = &href[start + 5..];
            let end = encoded.find('&').unwrap_or(encoded.len());
            let encoded_url = &encoded[..end];

            return match urlencoding::decode(encoded_url) {
                Ok(decoded) => Some(decoded.to_string()),
                Err(e) => {
                    log::trace!("Failed to decode URL '{}': {}", encoded_url, e);
                    None
                }
            };
        }

        if href.contains("duckduckgo.com") {
            return None;
        }

        if href.starts_with("http://") || href.starts_with("https://") {
            return Some(href.to_string());
        }

        if href.starts_with("//") {
            return Some(format!("https:{}", href));
        }

        log::trace!("Could not extract URL from: {}", href);
        None
    }

    /// Clears the search cache
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.lock().await;
        cache.clear();
        log::trace!("Search cache cleared");
    }
}

#[async_trait]
impl SearchProvider for DuckDuckGoSearchTool {
    async fn search(
        &self,
        query: &str,
        max_results: usize,
        region: &str,
    ) -> rustychain::Result<Vec<SearchResult>> {
        let args = DuckDuckGoSearchArgs {
            query: query.to_string(),
            max_results,
            region: region.to_string(),
            use_cache: true,
        };

        let response = DuckDuckGoSearchTool::search(self, args).await?;
        Ok(response.results)
    }

    fn name(&self) -> &'static str {
        "DuckDuckGo"
    }
}

#[async_trait]
impl FnExecutor<DuckDuckGoSearchArgs, DuckDuckGoSearchResponse> for DuckDuckGoSearchTool {
    async fn call(
        &self,
        args: DuckDuckGoSearchArgs,
    ) -> rustychain::Result<DuckDuckGoSearchResponse> {
        self.search(args).await
    }
}

impl FnDeclarator<DuckDuckGoSearchArgs, DuckDuckGoSearchResponse> for DuckDuckGoSearchTool {
    fn declare(&self) -> FunctionDeclaration<DuckDuckGoSearchArgs, DuckDuckGoSearchResponse> {
        FunctionDeclaration {
            name: "duckduckgo_search",
            description: "Perform a web search using DuckDuckGo and return the results. Supports caching, rate limiting, and URL validation.",
            parameters: schemars::schema_for!(DuckDuckGoSearchArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    // Helper to create a mock DuckDuckGo HTML response
    fn create_mock_html(num_results: usize) -> String {
        let mut html = String::from(r#"<!DOCTYPE html><html><body>"#);

        for i in 0..num_results {
            html.push_str(&format!(
                r#"<div class="result">
                    <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage{}">
                        Result Title {}
                    </a>
                    <a class="result__snippet">This is snippet number {}</a>
                </div>"#,
                i, i, i
            ));
        }

        html.push_str("</body></html>");
        html
    }

    #[test]
    fn test_user_agent_factory() {
        let chrome = UserAgentFactory::chrome();
        assert!(chrome.contains("Chrome"));
        assert!(chrome.contains("Mozilla/5.0"));

        let firefox = UserAgentFactory::firefox();
        assert!(firefox.contains("Firefox"));
        assert!(firefox.contains("Gecko"));

        let safari = UserAgentFactory::safari();
        assert!(safari.contains("Safari"));
        assert!(safari.contains("AppleWebKit"));

        let random = UserAgentFactory::random();
        assert!(!random.is_empty());
    }

    #[test]
    fn test_url_validator_valid() {
        let valid_urls = vec![
            "https://example.com/page",
            "http://example.com",
            "https://sub.domain.example.com/path?query=1",
        ];

        for url in valid_urls {
            assert!(
                UrlValidator::validate(url).is_some(),
                "Should accept: {}",
                url
            );
        }
    }

    #[test]
    fn test_url_validator_invalid_scheme() {
        let invalid_urls = vec![
            "javascript:alert(1)",
            "file:///etc/passwd",
            "ftp://example.com",
            "data:text/html,<script>alert(1)</script>",
        ];

        for url in invalid_urls {
            assert!(
                UrlValidator::validate(url).is_none(),
                "Should reject: {}",
                url
            );
        }
    }

    #[test]
    fn test_url_validator_private_ips() {
        let private_urls = vec![
            "http://localhost/admin",
            "http://127.0.0.1/",
            "http://10.0.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
        ];

        for url in private_urls {
            assert!(
                UrlValidator::validate(url).is_none(),
                "Should reject private IP: {}",
                url
            );
        }
    }

    #[test]
    fn test_url_sanitizer() {
        let url = "https://example.com/page?utm_source=test&utm_medium=email&id=123";
        let sanitized = UrlValidator::sanitize(url).unwrap();

        assert!(!sanitized.contains("utm_source"));
        assert!(!sanitized.contains("utm_medium"));
        assert!(sanitized.contains("id=123"));
    }

    #[test]
    fn test_args_validation_empty_query() {
        let args = DuckDuckGoSearchArgs {
            query: "   ".to_string(),
            max_results: 10,
            region: "wt-wt".to_string(),
            use_cache: true,
        };

        assert!(args.validate().is_err());
    }

    #[test]
    fn test_args_validation_max_results() {
        let args = DuckDuckGoSearchArgs {
            query: "test".to_string(),
            max_results: 0,
            region: "wt-wt".to_string(),
            use_cache: true,
        };
        assert!(args.validate().is_err());

        let args = DuckDuckGoSearchArgs {
            query: "test".to_string(),
            max_results: 101,
            region: "wt-wt".to_string(),
            use_cache: true,
        };
        assert!(args.validate().is_err());
    }

    #[test]
    fn test_args_validation_valid() {
        let args = DuckDuckGoSearchArgs {
            query: "rust programming".to_string(),
            max_results: 10,
            region: "wt-wt".to_string(),
            use_cache: true,
        };

        assert!(args.validate().is_ok());
    }

    #[test]
    fn test_extract_url_with_uddg() {
        let tool = DuckDuckGoSearchTool::new();
        let href = "//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fpage&rut=abc123";
        let url = tool.extract_url(href);
        assert_eq!(url, Some("https://example.com/page".to_string()));
    }

    #[test]
    fn test_extract_url_direct() {
        let tool = DuckDuckGoSearchTool::new();
        let href = "https://example.com/page";
        let url = tool.extract_url(href);
        assert_eq!(url, Some("https://example.com/page".to_string()));
    }

    #[test]
    fn test_extract_url_rejects_internal() {
        let tool = DuckDuckGoSearchTool::new();
        let href = "//duckduckgo.com/privacy";
        let url = tool.extract_url(href);
        assert!(url.is_none());
    }

    #[tokio::test]
    async fn test_search_with_mock() {
        let mut server = Server::new_async().await;

        let mock = server
            .mock("GET", "/html/")
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body(create_mock_html(5))
            .create_async()
            .await;
        // Make a request to the mock server so the mock is exercised
        let url = format!("{}/html/?q=test", server.url());
        let resp = reqwest::get(&url)
            .await
            .expect("failed to request mock server");
        let body = resp.text().await.expect("failed to read mock response");

        // Parse results using the tool's parser to ensure HTML format is handled
        let tool = DuckDuckGoSearchTool::new();
        let parsed = tool
            .parse_results(&body, 5)
            .expect("failed to parse results");
        assert_eq!(parsed.len(), 5);

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_cache_functionality() {
        let tool = DuckDuckGoSearchTool::new();

        // Clear cache first
        tool.clear_cache().await;

        let test_results = vec![SearchResult {
            title: "Test Result".to_string(),
            url: "https://example.com".to_string(),
            snippet: Some("Test snippet".to_string()),
        }];

        {
            let mut cache = tool.cache.lock().await;
            cache.insert(DuckDuckGoSearchCached {
                params: DuckDuckGoSearchArgs {
                    query: "test".to_string(),
                    max_results: 10,
                    region: "wt-wt".to_string(),
                    use_cache: true,
                },
                results: test_results.clone(),
            });
        }

        // Verify cache contains the item
        {
            let cache = tool.cache.lock().await;
            assert!(cache.front().is_some());
        }

        // Clear and verify
        tool.clear_cache().await;
        {
            let cache = tool.cache.lock().await;
            assert!(cache.front().is_none());
        }
    }
}
