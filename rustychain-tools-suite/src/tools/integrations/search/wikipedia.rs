//! wikipedia
use reqwest::Client;
use rustychain::FunctionDeclaration;
use rustychain::llm::function::{FnDeclarator, FnExecutor};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct WikipediaArgs {
    #[schemars(description = "The title or search term for the article.")]
    pub title: String,
    #[schemars(description = "Language code (e.g., 'en', 'es'). Default is 'en'.")]
    pub language: Option<String>,
}

#[derive(Clone, Default)]
pub struct WikipediaTool {
    client: Client,
}

impl WikipediaTool {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl FnExecutor<WikipediaArgs, serde_json::Value> for WikipediaTool {
    async fn call(&self, args: WikipediaArgs) -> rustychain::Result<serde_json::Value> {
        let lang = args.language.unwrap_or_else(|| "en".to_string());
        let url = format!(
            "https://{}.wikipedia.org/api/rest_v1/page/summary/{}",
            lang,
            urlencoding::encode(&args.title.replace(' ', "_"))
        );

        let response = self.client.get(url).send().await.map_err(|e| {
            rustychain::Error::Internal(format!("Failed to fetch Wikipedia article: {}", e).into())
        })?;

        if !response.status().is_success() {
            return Ok(serde_json::json!({ "error": "Article not found" }));
        }

        let data = response.json::<serde_json::Value>().await.map_err(|e| {
            rustychain::Error::Internal(format!("Failed to parse Wikipedia response: {}", e).into())
        })?;

        Ok(serde_json::json!({
            "title": data["title"],
            "extract": data["extract"],
            "description": data["description"],
            "url": data["content_urls"]["desktop"]["page"]
        }))
    }
}

impl FnDeclarator<WikipediaArgs, serde_json::Value> for WikipediaTool {
    fn declare(&self) -> FunctionDeclaration<WikipediaArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "wikipedia_tool",
            description: "Fetches structured summaries from Wikipedia articles.",
            parameters: schema_for!(WikipediaArgs),
            executor: Arc::new(self.clone()),
        }
    }
}
