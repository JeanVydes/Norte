use rustychain::{
    FnDeclarator, FnExecutor, FunctionDeclaration,
    prelude::{Cleaner, Formatter},
    util::{
        UserAgentFactory,
        formatters::{html::HtmlFormatter, whitespace::WhitespaceFormatter},
    },
};
use schemars::JsonSchema;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionMode {
    #[default]
    #[schemars(description = "Returns the complete HTML structure of the page.")]
    Full,
    #[schemars(
        description = "Uses a readability algorithm to extract the main article content. Best for blogs and news."
    )]
    Readability,
    #[schemars(description = "Extracts only the elements matching the provided CSS selectors.")]
    Selective,
    #[schemars(description = "Strips all HTML tags and returns only the visible text.")]
    TextOnly,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone, Copy, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputFormat {
    #[default]
    Html,
    Markdown,
    PlainText,
    Json,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug)]
pub struct ScrappingArgs {
    #[schemars(description = "The absolute URL to scrape (must start with http or https).")]
    pub url: String,
    #[schemars(description = "Configuration for the scraper behavior.")]
    pub options: Option<ScrappingOptions>,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug, Default)]
pub struct ScrappingOptions {
    #[schemars(
        description = "The method used to extract content. 'readability' is recommended for content analysis."
    )]
    pub mode: Option<ExtractionMode>,

    #[schemars(description = "The desired format of the resulting 'content' field.")]
    pub output_format: Option<OutputFormat>,

    #[schemars(description = "Override the default User-Agent header.")]
    pub user_agent: Option<String>,

    #[schemars(description = "List of CSS selectors to include (only for 'selective' mode).")]
    pub css_selectors: Option<Vec<String>>,

    #[schemars(description = "List of CSS selectors to remove from the final output.")]
    pub exclude_selectors: Option<Vec<String>>,

    #[schemars(description = "If true, extracts title, description, and OpenGraph tags.")]
    pub include_metadata: Option<bool>,

    #[schemars(description = "If true, returns a list of all hyperlinks found in the page.")]
    pub extract_links: Option<bool>,

    #[schemars(
        description = "If true, returns a list of all images with their alt text and URLs."
    )]
    pub extract_images: Option<bool>,

    #[schemars(description = "If true, extracts and parses HTML tables into structured rows.")]
    pub extract_tables: Option<bool>,

    #[schemars(
        description = "Maximum length of the extracted content string. Useful to save context tokens."
    )]
    pub max_length: Option<usize>,

    #[schemars(description = "If true, removes redundant whitespace and empty tags.")]
    pub clean_content: Option<bool>,

    #[schemars(description = "Timeout for the HTTP request in seconds (default is 30).")]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct ScrappingTool;

impl ScrappingTool {
    pub fn new() -> Self {
        Self
    }

    #[warn(clippy::useless_conversion)]
    async fn get_client(&self, options: &ScrappingOptions) -> rustychain::Result<reqwest::Client> {
        let user_agent = options
            .user_agent
            .clone()
            .unwrap_or_else(UserAgentFactory::random);
        let timeout = Duration::from_secs(options.timeout_secs.unwrap_or(30));

        reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::limited(10))
            .build()
            .map_err(|e| {
                rustychain::Error::Internal(format!("Failed to build HTTP client: {}", e).into())
            })
    }

    pub async fn scrape(&self, args: ScrappingArgs) -> rustychain::Result<ScrappingResult> {
        let url = url::Url::parse(&args.url)?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(rustychain::Error::Validation {
                message: "URL must use HTTP or HTTPS protocol".to_string(),
            });
        }

        let options = args.options.unwrap_or_default();
        let client = self.get_client(&options).await?;
        let response = client.get(url.as_str()).send().await.map_err(|e| {
            rustychain::Error::Internal(format!("HTTP request failed: {}", e).into())
        })?;

        if !response.status().is_success() {
            return Err(rustychain::Error::HTTP(
                format!("HTTP {}: {}", response.status(), url).into(),
            ));
        }

        let html = response.text().await.unwrap_or_default();
        if html.is_empty() {
            return Err(rustychain::Error::NoContent);
        }

        let document = Html::parse_document(&html);
        let mode = options.mode.unwrap_or_default();

        let mut content = match mode {
            ExtractionMode::Full => self.extract_full(&document, &options)?,
            ExtractionMode::Readability => self.extract_readability(&document, &options)?,
            ExtractionMode::Selective => self.extract_selective(&document, &options)?,
            ExtractionMode::TextOnly => self.extract_text_only(&document, &options)?,
        };

        if let Some(max_len) = options.max_length
            && content.len() > max_len
        {
            content.truncate(max_len);
            content.push_str("... [truncated]");
        }

        let output_format = options.output_format.unwrap_or_default();

        Ok(ScrappingResult {
            content: self.format_output(&content, output_format)?,
            metadata: options
                .include_metadata
                .unwrap_or_default()
                .then(|| self.extract_metadata(&document, url.as_str())),
            links: options
                .extract_links
                .unwrap_or_default()
                .then(|| self.extract_links(&document, url.as_str())),
            images: options
                .extract_images
                .unwrap_or_default()
                .then(|| self.extract_images(&document, url.as_str())),
            tables: options
                .extract_tables
                .unwrap_or_default()
                .then(|| self.extract_tables(&document)),
            url: url.to_string(),
            format: output_format,
        })
    }

    fn extract_full(
        &self,
        document: &Html,
        options: &ScrappingOptions,
    ) -> rustychain::Result<String> {
        let mut html = document.html();
        if let Some(excludes) = &options.exclude_selectors {
            for sel in excludes {
                if let Ok(selector) = Selector::parse(sel) {
                    for element in document.select(&selector) {
                        html = html.replace(&element.html(), "");
                    }
                }
            }
        }
        Ok(if options.clean_content.unwrap_or(true) {
            WhitespaceFormatter::clean_html(&html)
        } else {
            html
        })
    }

    fn extract_readability(
        &self,
        document: &Html,
        options: &ScrappingOptions,
    ) -> rustychain::Result<String> {
        let content_selectors = [
            "article",
            "main",
            "[role='main']",
            ".article-content",
            ".post-content",
            "#content",
        ];
        let mut best_content = content_selectors
            .iter()
            .filter_map(|s| Selector::parse(s).ok())
            .filter_map(|s| {
                document
                    .select(&s)
                    .max_by_key(|e| e.text().collect::<String>().len())
            })
            .map(|e| e.html())
            .next()
            .unwrap_or_else(|| document.root_element().html());

        let noise = [
            "script",
            "style",
            "nav",
            "header",
            "footer",
            "aside",
            ".ads",
            ".comments",
        ];
        for sel in noise {
            if let Ok(selector) = Selector::parse(sel) {
                let fragment = Html::parse_fragment(&best_content);
                for element in fragment.select(&selector) {
                    best_content = best_content.replace(&element.html(), "");
                }
            }
        }

        Ok(if options.clean_content.unwrap_or(true) {
            WhitespaceFormatter::clean_html(&best_content)
        } else {
            best_content
        })
    }

    fn extract_selective(
        &self,
        document: &Html,
        options: &ScrappingOptions,
    ) -> rustychain::Result<String> {
        let selectors =
            options
                .css_selectors
                .as_ref()
                .ok_or_else(|| rustychain::Error::Validation {
                    message: "css_selectors required for selective mode".to_string(),
                })?;

        let mut extracted = String::new();
        for sel in selectors {
            if let Ok(selector) = Selector::parse(sel) {
                for element in document.select(&selector) {
                    extracted.push_str(&element.html());
                    extracted.push('\n');
                }
            }
        }
        Ok(if options.clean_content.unwrap_or(true) {
            WhitespaceFormatter::clean_html(&extracted)
        } else {
            extracted
        })
    }

    fn extract_text_only(
        &self,
        _document: &Html,
        options: &ScrappingOptions,
    ) -> rustychain::Result<String> {
        let text: String = _document
            .root_element()
            .text()
            .filter(|t| !t.trim().is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        Ok(if options.clean_content.unwrap_or(true) {
            WhitespaceFormatter::clean_text(&text)
        } else {
            text
        })
    }

    fn extract_metadata(&self, document: &Html, url: &str) -> PageMetadata {
        let mut meta = PageMetadata {
            url: url.to_string(),
            ..Default::default()
        };

        if let Ok(sel) = Selector::parse("title")
            && let Some(t) = document.select(&sel).next()
        {
            meta.title = Some(t.text().collect::<String>().trim().to_string());
        }

        if let Ok(sel) = Selector::parse("meta") {
            for m in document.select(&sel) {
                let name = m.value().attr("name").map(|s| s.to_lowercase());
                let prop = m.value().attr("property").map(|s| s.to_lowercase());
                let cont = m.value().attr("content").map(|s| s.to_string());

                if let Some(c) = cont {
                    match name.as_deref() {
                        Some("description") => meta.description = Some(c.clone()),
                        Some("author") => meta.author = Some(c.clone()),
                        Some("keywords") => meta.keywords = Some(c.clone()),
                        _ => {}
                    }
                    match prop.as_deref() {
                        Some("og:title") if meta.title.is_none() => meta.title = Some(c),
                        Some("og:description") if meta.description.is_none() => {
                            meta.description = Some(c)
                        }
                        Some("og:image") => meta.image = Some(c),
                        _ => {}
                    }
                }
            }
        }

        if let Ok(sel) = Selector::parse("html")
            && let Some(h) = document.select(&sel).next()
            && let Some(l) = h.value().attr("lang")
        {
            meta.language = Some(l.to_string());
        }
        meta
    }

    fn extract_links(&self, document: &Html, base_url: &str) -> Vec<LinkInfo> {
        let base = url::Url::parse(base_url).ok();
        Selector::parse("a[href]")
            .map(|sel| {
                document
                    .select(&sel)
                    .filter_map(|e| {
                        let href = e.value().attr("href")?;
                        let url = base.as_ref()?.join(href).ok()?.to_string();
                        let text = e.text().collect::<String>().trim().to_string();
                        Some(LinkInfo {
                            url,
                            text: (!text.is_empty()).then_some(text),
                            title: e.value().attr("title").map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn extract_images(&self, document: &Html, base_url: &str) -> Vec<ImageInfo> {
        let base = url::Url::parse(base_url).ok();
        Selector::parse("img")
            .map(|sel| {
                document
                    .select(&sel)
                    .filter_map(|e| {
                        let src = e.value().attr("src")?;
                        let url = base.as_ref()?.join(src).ok()?.to_string();
                        Some(ImageInfo {
                            url,
                            alt: e.value().attr("alt").map(String::from),
                            title: e.value().attr("title").map(String::from),
                            width: e.value().attr("width").map(String::from),
                            height: e.value().attr("height").map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn extract_tables(&self, document: &Html) -> Vec<TableData> {
        Selector::parse("table")
            .map(|sel| {
                document
                    .select(&sel)
                    .enumerate()
                    .map(|(index, table)| {
                        let rows = Selector::parse("tr")
                            .map(|r_sel| {
                                table
                                    .select(&r_sel)
                                    .map(|row| {
                                        Selector::parse("th, td")
                                            .map(|c_sel| {
                                                row.select(&c_sel)
                                                    .map(|c| {
                                                        c.text()
                                                            .collect::<String>()
                                                            .trim()
                                                            .to_string()
                                                    })
                                                    .collect::<Vec<_>>()
                                            })
                                            .unwrap_or_default()
                                    })
                                    .filter(|r| !r.is_empty())
                                    .collect()
                            })
                            .unwrap_or_default();
                        TableData {
                            index,
                            rows,
                            caption: None,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn format_output(&self, content: &str, format: OutputFormat) -> rustychain::Result<String> {
        match format {
            OutputFormat::Html | OutputFormat::Json => Ok(content.to_string()),
            OutputFormat::Markdown => HtmlFormatter::to_markdown(content),
            OutputFormat::PlainText => {
                let doc = Html::parse_fragment(content);
                let text = doc.root_element().text().collect::<Vec<_>>().join(" ");
                Ok(WhitespaceFormatter::clean_text(&text))
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrappingResult {
    pub content: String,
    pub metadata: Option<PageMetadata>,
    pub links: Option<Vec<LinkInfo>>,
    pub images: Option<Vec<ImageInfo>>,
    pub tables: Option<Vec<TableData>>,
    pub url: String,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PageMetadata {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub author: Option<String>,
    pub keywords: Option<String>,
    pub image: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkInfo {
    pub url: String,
    pub text: Option<String>,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageInfo {
    pub url: String,
    pub alt: Option<String>,
    pub title: Option<String>,
    pub width: Option<String>,
    pub height: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableData {
    pub index: usize,
    pub rows: Vec<Vec<String>>,
    pub caption: Option<String>,
}

#[async_trait::async_trait]
impl FnExecutor<ScrappingArgs, ScrappingResult> for ScrappingTool {
    async fn call(&self, args: ScrappingArgs) -> rustychain::Result<ScrappingResult> {
        self.scrape(args).await
    }
}

impl FnDeclarator<ScrappingArgs, ScrappingResult> for ScrappingTool {
    fn declare(&self) -> FunctionDeclaration<ScrappingArgs, ScrappingResult> {
        FunctionDeclaration {
            name: "scrape_web",
            description: "A comprehensive web scraping tool for extracting content from websites. Use 'readability' mode for articles, documentation, or news. You can also extract metadata, links, images, and tables independently.",
            parameters: schemars::schema_for!(ScrappingArgs),
            executor: std::sync::Arc::new(self.clone()),
        }
    }
}
