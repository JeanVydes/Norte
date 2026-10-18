//! The write executor: whole-file creation and replacement (FR-TOOL-WRITE-1,
//! FR-TOOL-WRITE-2).

use std::path::PathBuf;

use rustychain_core::{
    Digest, Error, FnExecutor, Part, Result, ToolDeclaration, ToolMetadata, ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::fs::{write_atomic, write_new};

/// A whole file, written in one call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteRequest {
    /// The file to write. Parent directories are created.
    #[schemars(example = &"src/greeting.rs")]
    pub path: PathBuf,
    /// The complete contents of the file, verbatim.
    pub content: String,
    /// `true` replaces a file that already exists. `false` makes an existing
    /// file a failed call instead of a replacement.
    #[schemars(
        description = "true replaces a file that already exists; false makes an existing file a failed call.",
        example = false
    )]
    pub overwrite: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum WriteCode {
    /// The file did not exist and now does.
    Created,
    /// The file existed, `overwrite` was set, and it was replaced.
    Overwritten,
    /// The file existed and `overwrite` was not set. Nothing was written.
    Exists,
}

/// What one write did. The digest fields are absent when the call refused to
/// replace an existing file, because then nothing on disk changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOutput {
    pub code: WriteCode,
    pub path: PathBuf,
    pub existed: bool,
    pub created_directories: bool,
    pub bytes: Option<usize>,
    pub lines: Option<usize>,
    pub content_digest: Option<Digest>,
    pub request_digest: Digest,
}

/// Bounds for the write executor (FR-TOOL-FILE-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WriteLimits {
    /// Refuse content larger than this.
    pub max_bytes: usize,
}

impl Default for WriteLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Creates or replaces a text file (FR-TOOL-WRITE-1).
///
/// The path comes from the request: what may be written is the host's policy
/// (FR-POL-1), not the executor's business.
#[rustychain_core::tool(
    name = "write",
    description = "Write a whole text file. Creates the parent directories. Set overwrite to true to replace a file that already exists; with it false, an existing file makes the call fail. Use edit to change part of a file."
)]
pub struct WriteTool {
    limits: WriteLimits,
}

impl WriteTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: WriteLimits::default(),
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: WriteLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<WriteRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, std::sync::Arc::new(self))
    }

    fn run(&self, request: &WriteRequest) -> Result<ToolOutput> {
        if request.path.as_os_str().is_empty() {
            return Err(Error::InvalidInput("write: a path is required".to_owned()));
        }
        if request.content.len() > self.limits.max_bytes {
            return Err(Error::LimitExceeded {
                what: request.path.display().to_string(),
                limit: self.limits.max_bytes,
            });
        }
        let created_directories = parents_missing(&request.path);
        let request_digest = Digest::of_json(&serde_json::to_value(request)?);

        // With `overwrite`, the file is replaced; without it, the creation is
        // itself the check, so a file that appears once this call has started is
        // not clobbered by a rename (FR-TOOL-WRITE-1).
        let created = if request.overwrite {
            let existed = request.path.exists();
            write_atomic(&request.path, request.content.as_bytes())?;
            !existed
        } else {
            write_new(&request.path, request.content.as_bytes())?
        };
        if !created && !request.overwrite {
            let output = WriteOutput {
                code: WriteCode::Exists,
                path: request.path.clone(),
                existed: true,
                created_directories: false,
                bytes: None,
                lines: None,
                content_digest: None,
                request_digest,
            };
            return render(&output);
        }

        let output = WriteOutput {
            code: if created {
                WriteCode::Created
            } else {
                WriteCode::Overwritten
            },
            path: request.path.clone(),
            existed: !created,
            created_directories,
            bytes: Some(request.content.len()),
            lines: Some(request.content.lines().count()),
            content_digest: Some(Digest::of(request.content.as_bytes())),
            request_digest,
        };
        render(&output)
    }
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<WriteRequest, ToolOutput> for WriteTool {
    async fn call(&self, request: WriteRequest) -> Result<ToolOutput> {
        self.run(&request)
    }
}

/// Convenience: build the suite's file writer as an erased function.
#[must_use]
pub fn write_tool() -> std::sync::Arc<dyn rustychain_core::AnyFunction> {
    std::sync::Arc::new(WriteTool::new().into_declaration())
}

/// Whether any directory between the root and the file has to be created.
fn parents_missing(path: &std::path::Path) -> bool {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .is_some_and(|parent| !parent.exists())
}

fn render(output: &WriteOutput) -> Result<ToolOutput> {
    let body = match output.code {
        WriteCode::Created | WriteCode::Overwritten => {
            let verb = if output.code == WriteCode::Created {
                "wrote"
            } else {
                "replaced"
            };
            let bytes = output.bytes.unwrap_or_default();
            let lines = output.lines.unwrap_or_default();
            let directories = if output.created_directories {
                "; parent directories created"
            } else {
                ""
            };
            format!(
                "{verb} {} ({lines} lines, {bytes} bytes){directories}",
                output.path.display()
            )
        }
        WriteCode::Exists => format!(
            "write refused: {} already exists and overwrite is false; \
             pass overwrite=true to replace it",
            output.path.display()
        ),
    };
    audit(output);
    Ok(ToolOutput {
        parts: vec![Part::text(body)],
        structured: Some(serde_json::to_value(output)?),
        is_error: output.code == WriteCode::Exists,
    })
}

fn audit(output: &WriteOutput) {
    tracing::info!(
        target: "rustychain.audit",
        tool = "write",
        code = ?output.code,
        path = %output.path.display(),
        bytes = output.bytes,
        content_digest = ?output.content_digest.map(|digest| digest.to_string()),
        request_digest = %output.request_digest,
        "file written"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    fn scratch(name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ticket = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "rustychain-write-{name}-{}-{ticket}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory).expect("scratch");
        directory
    }

    fn request(path: &std::path::Path, content: &str, overwrite: bool) -> WriteRequest {
        WriteRequest {
            path: path.to_path_buf(),
            content: content.to_owned(),
            overwrite,
        }
    }

    fn structured(output: &ToolOutput) -> serde_json::Value {
        output.structured.clone().expect("structured")
    }

    #[tokio::test]
    async fn creates_a_file_and_its_parent_directories() {
        let directory = scratch("create");
        let path = directory.join("src").join("config.rs");
        let output = WriteTool::new()
            .call(request(&path, "pub struct Config;\n", false))
            .await
            .expect("write");
        assert!(!output.is_error, "creating a file is not a failure");
        let structured = structured(&output);
        assert_eq!(structured["code"], "created");
        assert_eq!(structured["existed"], false);
        assert_eq!(structured["created_directories"], true);
        assert_eq!(structured["lines"], 1);
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "pub struct Config;\n"
        );
        assert!(structured["content_digest"].is_string());
    }

    #[tokio::test]
    async fn refuses_to_replace_a_file_without_overwrite() {
        let directory = scratch("refuse");
        let path = directory.join("kept.txt");
        std::fs::write(&path, "the original\n").expect("seed");
        let output = WriteTool::new()
            .call(request(&path, "the replacement\n", false))
            .await
            .expect("refused");
        assert!(output.is_error, "a refusal is a failed result");
        let structured = structured(&output);
        assert_eq!(structured["code"], "exists");
        assert_eq!(structured["existed"], true);
        assert!(
            structured["bytes"].is_null(),
            "nothing was written, so there is nothing to report"
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "the original\n",
            "the file is untouched"
        );
    }

    #[tokio::test]
    async fn replaces_a_file_when_overwrite_is_set() {
        let directory = scratch("replace");
        let path = directory.join("kept.txt");
        std::fs::write(&path, "the original\n").expect("seed");
        let output = WriteTool::new()
            .call(request(&path, "the replacement\n", true))
            .await
            .expect("write");
        assert!(!output.is_error);
        assert_eq!(structured(&output)["code"], "overwritten");
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "the replacement\n"
        );
    }

    #[tokio::test]
    async fn writes_an_empty_file() {
        let directory = scratch("empty");
        let path = directory.join("empty.txt");
        let output = WriteTool::new()
            .call(request(&path, "", false))
            .await
            .expect("write");
        let structured = structured(&output);
        assert_eq!(structured["bytes"], 0);
        assert_eq!(structured["lines"], 0);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "");
    }

    #[tokio::test]
    async fn an_empty_path_is_invalid_input() {
        let error = WriteTool::new()
            .call(request(std::path::Path::new(""), "text", true))
            .await
            .expect_err("invalid");
        assert!(matches!(error, Error::InvalidInput(reason) if reason.contains("path")));
    }

    #[tokio::test]
    async fn writing_over_a_directory_names_the_path() {
        let directory = scratch("isdir");
        let error = WriteTool::new()
            .call(request(&directory, "text", true))
            .await
            .expect_err("directory");
        assert!(
            error
                .to_string()
                .contains(directory.to_string_lossy().as_ref())
        );
    }

    #[tokio::test]
    async fn a_write_leaves_no_temporary_file_behind() {
        let directory = scratch("clean");
        let path = directory.join("file.txt");
        WriteTool::new()
            .call(request(&path, "content", true))
            .await
            .expect("write");
        let entries = std::fs::read_dir(&directory).expect("dir").count();
        assert_eq!(entries, 1);
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&WriteTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("write").expect("name");
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(spec.description, <WriteTool as ToolMetadata>::DESCRIPTION);

        let directory = scratch("envelope");
        let path = directory.join("from-registry.txt");
        let output = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": path,
                    "content": "through the registry\n",
                    "overwrite": false
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        assert!(!output.parts.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "through the registry\n"
        );
    }

    #[tokio::test]
    async fn content_over_the_limit_is_refused_before_anything_is_written() {
        let directory = scratch("over-limit");
        let path = directory.join("big.txt");
        let tool = WriteTool::new().with_limits(WriteLimits { max_bytes: 4 });

        let error = tool
            .call(request(&path, "0123456789", false))
            .await
            .expect_err("over the limit");

        assert!(matches!(error, Error::LimitExceeded { limit: 4, .. }));
        assert!(!path.exists(), "the file was never created");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_of_two_concurrent_creations_wins() {
        let directory = scratch("two-writers");
        let path = directory.join("contested.txt");
        let tool = WriteTool::new();

        let first = tokio::spawn({
            let tool = tool.clone();
            let path = path.clone();
            async move { tool.call(request(&path, "one\n", false)).await }
        });
        let second = tokio::spawn({
            let tool = tool.clone();
            let path = path.clone();
            async move { tool.call(request(&path, "two\n", false)).await }
        });
        let first = first.await.expect("join").expect("call");
        let second = second.await.expect("join").expect("call");

        let winners = [&first, &second]
            .iter()
            .filter(|output| !output.is_error)
            .count();
        assert_eq!(winners, 1, "one file, one creator, whoever got there first");
        let content = std::fs::read_to_string(&path).expect("read");
        assert!(
            content == "one\n" || content == "two\n",
            "the loser wrote nothing over the winner: {content:?}"
        );
    }

    #[test]
    fn the_write_defaults_are_the_documented_ones() {
        assert_eq!(WriteLimits::default().max_bytes, 16 * 1024 * 1024);
    }

    #[tokio::test]
    async fn unknown_arguments_are_rejected_before_the_body_runs() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry.register_tool(&WriteTool::new()).expect("register");
        let name = rustychain_core::ToolName::new("write").expect("name");
        let error = registry
            .execute(
                &rustychain_core::ToolContext::new(),
                &name,
                serde_json::json!({
                    "path": "ignored.txt",
                    "content": "text",
                    "overwrite": false,
                    "mode": "append"
                }),
            )
            .await
            .expect_err("unknown field (FR-TOOL-2)");
        assert!(matches!(error, Error::InvalidToolArguments { .. }));
    }
}
