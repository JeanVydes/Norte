//! Declaring tools: three shapes, and the schema the model receives.
//!
//! A tool is a type that states its name, description and arguments, plus an
//! executor. Register it and the engine puts the arguments schema on the wire
//! with local `$ref`s already inlined (FR-TOOL-6), so a model that does not
//! follow references still sees the whole shape.
//!
//! No model and no key here: this recipe builds a registry and prints what would
//! go to the provider.
//!
//! ```sh
//! cargo run -p rustychain-cookbook --example tools
//! ```

use rustychain_core::{FnExecutor, Result, ToolMetadata, ToolOutput, ToolRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ── 1. The short shape: the `#[tool]` macro states name and description. ──

#[rustychain_core::tool(name = "word_count", description = "Count the words in a document")]
struct WordCount;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WordCountArgs {
    /// The document to count.
    ///
    /// A nested type: the engine replaces the local `$ref` with its definition
    /// before putting the schema on the wire, so a model that does not follow
    /// references still sees the whole shape (FR-TOOL-6).
    ///
    /// A `///` comment is the description the model reads. Where the developer's
    /// prose and the model's instruction differ, `#[schemars(description = ..)]`
    /// replaces it on the wire and the rustdoc stays for the reader;
    /// `#[schemars(example = ..)]` adds an entry to `examples`, which is how a
    /// nested shape gets shown rather than described.
    #[schemars(
        description = "The document whose words are counted.",
        example = serde_json::json!({"title": "Notes", "body": "one two three"})
    )]
    document: Document,
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Document {
    /// The document's title. Its words count too.
    title: String,
    /// The document's body.
    body: String,
}

#[derive(Debug, Serialize)]
struct WordCountResult {
    words: usize,
}

#[async_trait::async_trait]
impl FnExecutor<WordCountArgs, WordCountResult> for WordCount {
    async fn call(&self, args: WordCountArgs) -> Result<WordCountResult> {
        let Document { title, body } = args.document;
        Ok(WordCountResult {
            words: title.split_whitespace().count() + body.split_whitespace().count(),
        })
    }
}

// ── 2. The explicit shape: state name and description by hand instead. ──

#[derive(Clone, Copy)]
struct Now;

impl ToolMetadata for Now {
    const NAME: &'static str = "now";
    const DESCRIPTION: &'static str = "Report the host's time zone";
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct NowArgs {}

#[derive(Debug, Serialize)]
struct NowResult {
    zone: String,
}

#[async_trait::async_trait]
impl FnExecutor<NowArgs, NowResult> for Now {
    async fn call(&self, _args: NowArgs) -> Result<NowResult> {
        Ok(NowResult {
            zone: "UTC".to_owned(),
        })
    }
}

// ── 3. The rich shape: return a `ToolOutput` to carry structured data. ──

#[derive(Clone, Copy)]
struct ReadFile;

impl ToolMetadata for ReadFile {
    const NAME: &'static str = "read_file";
    const DESCRIPTION: &'static str = "Read a file, or report why it cannot be read";
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileArgs {
    /// The path to read, relative to the host's working directory.
    #[schemars(example = &"src/main.rs")]
    path: String,
}

#[async_trait::async_trait]
impl FnExecutor<ReadFileArgs, ToolOutput> for ReadFile {
    async fn call(&self, args: ReadFileArgs) -> Result<ToolOutput> {
        if args.path.starts_with('/') {
            // A refused call is still a normal result: the model gets it back and
            // can adapt. `ToolOutput::error` marks it as a failure.
            return Ok(ToolOutput::error(format!(
                "refusing the absolute path {}",
                args.path
            )));
        }
        Ok(ToolOutput::json(
            serde_json::json!({ "path": args.path, "bytes": 0 }),
        ))
    }
}

fn main() -> Result<()> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&WordCount)?;
    tools.register_declarator(&Now)?;
    tools.register_tool(&ReadFile)?;

    for spec in tools.specs() {
        println!(
            "{}\n  {}\n  schema: {}\n",
            spec.name, spec.description, spec.parameters
        );
    }
    Ok(())
}
