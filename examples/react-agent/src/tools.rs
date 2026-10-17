//! The example's own tool: fetch the `models.dev` catalogue.

use std::sync::Arc;

use rustychain_core::{Error, FnExecutor, Result, ToolName};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{self, Cache};

/// `fetch_catalog` takes no arguments.
#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FetchCatalogArgs {}

/// What the agent receives: how big the catalogue is, plus a taste of it. The
/// full document stays host-side for the fan-out step.
#[derive(Debug, Serialize)]
pub(crate) struct FetchCatalogResult {
    providers: usize,
    models: usize,
    providers_preview: Vec<String>,
}

/// Fetches the catalogue over HTTP and caches it host-side.
#[rustychain_core::tool(
    name = "fetch_catalog",
    description = "Download the models.dev catalogue: a JSON document listing AI providers and the models each one serves. Returns how many providers and models it contains, plus a short preview. The full document is retained host-side for later analysis."
)]
pub(crate) struct FetchCatalog {
    client: reqwest::Client,
    url: String,
    cache: Arc<Cache>,
}

impl FetchCatalog {
    /// Builds the tool around a shared cache.
    pub(crate) fn new(client: reqwest::Client, url: String, cache: Arc<Cache>) -> Self {
        Self { client, url, cache }
    }

    async fn fetch(&self) -> Result<Value> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await
            .map_err(|error| failure(&format!("GET {} failed: {error}", self.url)))?;
        let status = response.status();
        if !status.is_success() {
            return Err(failure(&format!("GET {} returned {status}", self.url)));
        }
        response
            .json::<Value>()
            .await
            .map_err(|error| failure(&format!("GET {} returned invalid JSON: {error}", self.url)))
    }
}

#[async_trait::async_trait]
impl FnExecutor<FetchCatalogArgs, FetchCatalogResult> for FetchCatalog {
    async fn call(&self, _args: FetchCatalogArgs) -> Result<FetchCatalogResult> {
        let catalog = self.fetch().await?;
        let (providers, models) = catalog::counts(&catalog);
        let providers_preview = preview(&catalog);
        self.cache.store(catalog);
        Ok(FetchCatalogResult {
            providers,
            models,
            providers_preview,
        })
    }
}

fn failure(reason: &str) -> Error {
    Error::ToolFailed {
        tool: ToolName::new("fetch_catalog").expect("static tool name"),
        reason: reason.to_owned(),
        code: Some("fetch_failed".to_owned()),
    }
}

fn preview(catalog: &Value) -> Vec<String> {
    catalog
        .pointer("/providers")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .take(12)
        .map(|(id, provider)| {
            provider
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_owned()
        })
        .collect()
}
