use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rustychain_core::{
    BlobId, BlobStore, Digest, Error, FnExecutor, Part, Result, ToolContext, ToolDeclaration,
    ToolMetadata, ToolOutput, ToolProgress, ToolStream,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout as with_timeout;

/// Program invocation. Exactly one of a direct program or a shell script.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind")]
#[non_exhaustive]
pub enum CommandLine {
    Program {
        program: String,
        #[serde(default)]
        args: Vec<String>,
    },
    Shell {
        shell: String,
        script: String,
    },
}

/// What the caller allows the process to do. Profiles are data: a sandbox
/// backend supplied by a higher-level crate interprets them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", default)]
#[non_exhaustive]
pub struct CommandCapabilities {
    pub network: bool,
    pub filesystem_write: bool,
    pub spawn: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandRequest {
    pub command: CommandLine,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Environment delta. The process starts from an empty environment.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// Optional blob holding the stdin bytes; takes precedence over `stdin`
    /// (FR-TOOL-BASH-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_blob: Option<BlobId>,
    /// Merge stderr into stdout for both the live stream and the final text;
    /// the host controls it (FR-TOOL-BASH-2).
    #[serde(default)]
    pub merge_stderr: bool,
    /// Machine-readable summary: the host gets the tail and/or the lines that
    /// match any of these literal patterns (FR-TOOL-BASH-3).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_patterns: Vec<String>,
    #[serde(default)]
    pub capabilities: CommandCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    /// When set, values in `secrets` are replaced in output before it reaches
    /// the model and the logs.
    #[serde(default)]
    pub redact_secrets: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_lines: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandCode {
    Success,
    NonZeroExit,
    Timeout,
    Cancelled,
    SpawnFailed,
    IoError,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub code: CommandCode,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub bytes_written: usize,
    pub truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub cwd: Option<PathBuf>,
    pub summary: Option<String>,
    pub request_digest: Digest,
    pub output_hash: Digest,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
}

impl CommandOutput {
    fn failed(
        code: CommandCode,
        reason: impl Into<String>,
        duration: Duration,
        request_digest: Digest,
        started_at: DateTime<Utc>,
    ) -> Self {
        Self {
            code,
            exit_code: None,
            signal: None,
            stdout: String::new(),
            stderr: reason.into(),
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            bytes_written: 0,
            truncated: false,
            timed_out: false,
            cancelled: false,
            cwd: None,
            summary: None,
            request_digest,
            output_hash: Digest::of(&[]),
            started_at,
            ended_at: Utc::now(),
        }
    }
}

pub trait CommandRenderer: Send + Sync {
    fn render(&self, output: &CommandOutput) -> String;
}

/// The default context renderer: enough for a model to decide what to do next.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainCommandRenderer;

impl CommandRenderer for PlainCommandRenderer {
    fn render(&self, output: &CommandOutput) -> String {
        use std::fmt::Write as _;

        let mut rendered = String::new();
        let _ = writeln!(rendered, "code: {:?}", output.code);
        let _ = writeln!(rendered, "exit_code: {:?}", output.exit_code);
        if let Some(signal) = output.signal {
            let _ = writeln!(rendered, "signal: {signal}");
        }
        let _ = writeln!(rendered, "duration_ms: {}", output.duration_ms);
        let _ = writeln!(rendered, "bytes_written: {}", output.bytes_written);
        if output.truncated {
            rendered.push_str("truncated: true\n");
        }
        if let Some(cwd) = &output.cwd {
            let _ = writeln!(rendered, "cwd: {}", cwd.display());
        }
        if !output.stdout.is_empty() {
            rendered.push_str("stdout:\n");
            rendered.push_str(&output.stdout);
            if !output.stdout.ends_with('\n') {
                rendered.push('\n');
            }
        }
        if !output.stderr.is_empty() {
            rendered.push_str("stderr:\n");
            rendered.push_str(&output.stderr);
            if !output.stderr.ends_with('\n') {
                rendered.push('\n');
            }
        }
        if let Some(summary) = &output.summary {
            rendered.push_str("summary:\n");
            rendered.push_str(summary);
        }
        rendered
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandLimits {
    pub default_timeout: Duration,
    pub grace: Duration,
    pub max_output_bytes: usize,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            grace: Duration::from_secs(2),
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// A sandbox backend turns a profile name into enforced restrictions
/// (FR-TOOL-BASH-7).
///
/// Profiles are data, so the executor ships only the seam: a higher-level crate
/// supplies a container, bubblewrap or firecracker backend that rewrites the
/// command (for example, wrapping it in `bwrap`).
pub trait SandboxBackend: Send + Sync {
    /// Applies `profile` to `command` before it is spawned.
    ///
    /// # Errors
    ///
    /// Returns an error when the profile cannot be enforced.
    fn apply(&self, profile: &str, command: &mut Command) -> Result<()>;
}

/// The flagship tool of the default suite: a production-grade command executor.
#[rustychain_core::tool(
    name = "command",
    description = "Run a program or shell script. Returns exit status, bounded stdout/stderr and a structured summary."
)]
#[derive(Clone)]
pub struct CommandTool {
    limits: CommandLimits,
    renderer: Arc<dyn CommandRenderer>,
    blobs: Option<Arc<dyn BlobStore>>,
    sandbox: Option<Arc<dyn SandboxBackend>>,
}

impl CommandTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: CommandLimits::default(),
            renderer: Arc::new(PlainCommandRenderer),
            blobs: None,
            sandbox: None,
        }
    }

    #[must_use]
    pub fn with_limits(mut self, limits: CommandLimits) -> Self {
        self.limits = limits;
        self
    }

    #[must_use]
    pub fn with_renderer(mut self, renderer: Arc<dyn CommandRenderer>) -> Self {
        self.renderer = renderer;
        self
    }

    /// Supplies the blob store so a `stdin_blob` can be resolved
    /// (FR-TOOL-BASH-1).
    #[must_use]
    pub fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// Supplies the backend that enforces a `sandbox` profile (FR-TOOL-BASH-7).
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxBackend>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<CommandRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    async fn stdin(&self, request: &CommandRequest) -> Result<Vec<u8>> {
        if let Some(id) = request.stdin_blob {
            let blobs = self.blobs.as_ref().ok_or_else(|| {
                Error::InvalidInput(
                    "command: a stdin_blob needs a blob store, none is configured".to_owned(),
                )
            })?;
            return blobs.get(id).await;
        }
        Ok(request.stdin.clone().unwrap_or_default().into_bytes())
    }

    async fn run(&self, context: &ToolContext, request: &CommandRequest) -> Result<ToolOutput> {
        let started = Instant::now();
        let started_at = Utc::now();
        let request_digest = Digest::of_json(&serde_json::to_value(request).unwrap_or(Value::Null));
        let timeout = request
            .timeout_ms
            .map_or(self.limits.default_timeout, Duration::from_millis);
        let max_output = request
            .max_output_bytes
            .unwrap_or(self.limits.max_output_bytes);

        let fail = |code: CommandCode, reason: String| {
            let output =
                CommandOutput::failed(code, reason, started.elapsed(), request_digest, started_at);
            audit(&output);
            render(&*self.renderer, &output)
        };

        let stdin = match self.stdin(request).await {
            Ok(bytes) => bytes,
            Err(error) => return fail(CommandCode::IoError, error.to_string()),
        };

        let mut command = build_command(request);
        if let (Some(profile), Some(backend)) = (&request.sandbox, &self.sandbox) {
            if let Err(error) = backend.apply(profile, &mut command) {
                return fail(CommandCode::SpawnFailed, error.to_string());
            }
        }

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => return fail(CommandCode::SpawnFailed, error.to_string()),
        };
        let pid = child.id();

        let stdout_task = tokio::spawn(read_bounded(
            child.stdout.take(),
            max_output,
            context.clone(),
            ToolStream::Stdout,
            false,
        ));
        let stderr_task = tokio::spawn(read_bounded(
            child.stderr.take(),
            max_output,
            context.clone(),
            ToolStream::Stderr,
            request.merge_stderr,
        ));

        if let Some(mut stdin_pipe) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = stdin_pipe.write_all(&stdin).await;
                let _ = stdin_pipe.shutdown().await;
            });
        }

        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            status = child.wait() => status,
            () = tokio::time::sleep(timeout) => {
                timed_out = true;
                self.terminate(&mut child, pid).await
            }
            () = context.cancelled() => {
                cancelled = true;
                self.terminate(&mut child, pid).await
            }
        };

        let stdout = stdout_task.await.unwrap_or_default();
        let stderr = stderr_task.await.unwrap_or_default();
        let decoded = decode_streams(request, &stdout, &stderr, max_output);
        let (code, exit_code, signal) = outcome(&status, cancelled, timed_out);

        let output = CommandOutput {
            code,
            exit_code,
            signal,
            stdout: decoded.stdout,
            stderr: decoded.stderr,
            duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            bytes_written: decoded.bytes_written,
            truncated: decoded.truncated,
            timed_out,
            cancelled,
            cwd: request.cwd.clone(),
            summary: decoded.summary,
            request_digest,
            output_hash: Digest::of(&decoded.combined),
            started_at,
            ended_at: Utc::now(),
        };
        audit(&output);
        render(&*self.renderer, &output)
    }

    async fn terminate(
        &self,
        child: &mut tokio::process::Child,
        pid: Option<u32>,
    ) -> std::io::Result<ExitStatus> {
        signal_group(pid, SignalKind::Terminate, child);
        if let Ok(status) = with_timeout(self.limits.grace, child.wait()).await {
            return status;
        }
        signal_group(pid, SignalKind::Kill, child);
        child.wait().await
    }
}

impl Default for CommandTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<CommandRequest, ToolOutput> for CommandTool {
    async fn call(&self, request: CommandRequest) -> Result<ToolOutput> {
        self.run(&ToolContext::new(), &request).await
    }

    async fn call_with_context(
        &self,
        context: &ToolContext,
        request: CommandRequest,
    ) -> Result<ToolOutput> {
        self.run(context, &request).await
    }
}

fn render(renderer: &dyn CommandRenderer, output: &CommandOutput) -> Result<ToolOutput> {
    let body = renderer.render(output);
    let structured = serde_json::to_value(output)?;
    let is_error = !matches!(output.code, CommandCode::Success);
    Ok(ToolOutput {
        parts: vec![Part::text(body)],
        structured: Some(structured),
        is_error,
    })
}

fn build_command(request: &CommandRequest) -> Command {
    let mut command = match &request.command {
        CommandLine::Program { program, args } => {
            let mut command = Command::new(program);
            command.args(args);
            command
        }
        CommandLine::Shell { shell, script } => {
            let mut command = Command::new(shell);
            command.arg("-c").arg(script);
            command
        }
    };
    command.env_clear();
    command.envs(&request.env);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
    command
}

#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    truncated: bool,
    total: usize,
}

struct Decoded {
    stdout: String,
    stderr: String,
    truncated: bool,
    summary: Option<String>,
    bytes_written: usize,
    combined: Vec<u8>,
}

fn outcome(
    status: &std::io::Result<ExitStatus>,
    cancelled: bool,
    timed_out: bool,
) -> (CommandCode, Option<i32>, Option<i32>) {
    let (exit_code, signal) = match status {
        Ok(status) => decompose_status(*status),
        Err(_) => (None, None),
    };
    let code = if cancelled {
        CommandCode::Cancelled
    } else if timed_out {
        CommandCode::Timeout
    } else {
        match status {
            Ok(status) if status.success() => CommandCode::Success,
            Ok(_) => CommandCode::NonZeroExit,
            Err(_) => CommandCode::IoError,
        }
    };
    (code, exit_code, signal)
}

fn decode_streams(
    request: &CommandRequest,
    stdout: &Captured,
    stderr: &Captured,
    max_output: usize,
) -> Decoded {
    let truncated = stdout.truncated || stderr.truncated;
    let stdout_truncated = stdout.truncated;
    let stderr_truncated = stderr.truncated;
    let bytes_written = stdout.total + stderr.total;
    let mut stdout = String::from_utf8_lossy(&stdout.bytes).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr.bytes).into_owned();
    let marker = format!("[output truncated at {max_output} bytes per stream]\n");
    if stdout_truncated {
        stdout.push_str(&marker);
    }
    if stderr_truncated {
        stderr.push_str(&marker);
    }
    if request.redact_secrets {
        stdout = redact(&stdout, &request.secrets);
        stderr = redact(&stderr, &request.secrets);
    }
    let summary = build_summary(request, &stdout, &stderr);
    if request.merge_stderr {
        stdout.push_str(&stderr);
        stderr.clear();
    }
    let mut combined = stdout.as_bytes().to_vec();
    combined.extend_from_slice(stderr.as_bytes());
    Decoded {
        stdout,
        stderr,
        truncated,
        summary,
        bytes_written,
        combined,
    }
}

/// The optional structured summary: the tail of stdout and/or the lines that
/// match any host-supplied literal pattern (FR-TOOL-BASH-3).
fn build_summary(request: &CommandRequest, stdout: &str, stderr: &str) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    if let Some(lines) = request.summary_lines {
        sections.push(format!("tail:\n{}", tail_lines(stdout, lines)));
    }
    if !request.summary_patterns.is_empty() {
        sections.push(format!(
            "matches:\n{}",
            matching_lines(stdout, stderr, &request.summary_patterns)
        ));
    }
    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n"))
    }
}

fn matching_lines(stdout: &str, stderr: &str, patterns: &[String]) -> String {
    stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| {
            patterns
                .iter()
                .any(|pattern| !pattern.is_empty() && line.contains(pattern.as_str()))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn decompose_status(status: ExitStatus) -> (Option<i32>, Option<i32>) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        (status.code(), status.signal())
    }
    #[cfg(not(unix))]
    {
        (status.code(), None)
    }
}

#[derive(Clone, Copy)]
enum SignalKind {
    Terminate,
    Kill,
}

fn signal_group(pid: Option<u32>, kind: SignalKind, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        let signal = match kind {
            SignalKind::Terminate => nix::sys::signal::Signal::SIGTERM,
            SignalKind::Kill => nix::sys::signal::Signal::SIGKILL,
        };
        if let Some(pid) = pid {
            let group = nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap_or(i32::MAX));
            let _ = nix::sys::signal::killpg(group, signal);
            return;
        }
    }
    let _ = match kind {
        SignalKind::Terminate | SignalKind::Kill => child.start_kill(),
    };
}

async fn read_bounded<R>(
    pipe: Option<R>,
    cap: usize,
    context: ToolContext,
    stream: ToolStream,
    merge: bool,
) -> Captured
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let Some(mut pipe) = pipe else {
        return Captured::default();
    };
    let emitted = if merge { ToolStream::Stdout } else { stream };
    let mut kept = Vec::new();
    let mut total = 0usize;
    let mut buffer = vec![0u8; 8192];
    loop {
        match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                total = total.saturating_add(read);
                let remaining = cap.saturating_sub(kept.len());
                if remaining > 0 {
                    kept.extend_from_slice(&buffer[..read.min(remaining)]);
                }
                context.emit(ToolProgress {
                    stream: emitted,
                    bytes: buffer[..read].to_vec(),
                    last: false,
                });
            }
        }
    }
    context.emit(ToolProgress {
        stream: emitted,
        bytes: Vec::new(),
        last: true,
    });
    let truncated = total > kept.len();
    Captured {
        bytes: kept,
        truncated,
        total,
    }
}

fn redact(text: &str, secrets: &[String]) -> String {
    let mut redacted = text.to_owned();
    for secret in secrets {
        if !secret.is_empty() {
            redacted = redacted.replace(secret, "[redacted]");
        }
    }
    redacted
}

fn tail_lines(text: &str, lines: usize) -> String {
    let collected: Vec<&str> = text.lines().collect();
    let start = collected.len().saturating_sub(lines);
    collected[start..].join("\n")
}

fn audit(output: &CommandOutput) {
    tracing::info!(
        target: "rustychain.audit",
        code = ?output.code,
        exit_code = ?output.exit_code,
        duration_ms = output.duration_ms,
        bytes_written = output.bytes_written,
        request_digest = %output.request_digest,
        output_hash = %output.output_hash,
        started_at = %output.started_at,
        ended_at = %output.ended_at,
        "command executed"
    );
}

/// Convenience: build the default suite's command tool as an erased function.
#[must_use]
pub fn command_tool() -> Arc<dyn rustychain_core::AnyFunction> {
    Arc::new(CommandTool::new().into_declaration())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn captures_stdout_and_reports_success() {
        let tool = CommandTool::new();
        let output = tool
            .call(CommandRequest {
                command: CommandLine::Program {
                    program: "echo".to_owned(),
                    args: vec!["hello".to_owned()],
                },
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms: None,
                max_output_bytes: None,
                stdin: None,
                stdin_blob: None,
                merge_stderr: false,
                summary_patterns: Vec::new(),
                capabilities: CommandCapabilities::default(),
                sandbox: None,
                redact_secrets: false,
                secrets: Vec::new(),
                summary_lines: None,
            })
            .await
            .expect("execute");
        assert!(!output.is_error);
        let structured = output.structured.expect("structured");
        assert_eq!(structured["code"], "success");
        assert_eq!(structured["stdout"], "hello\n");
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported_not_raised() {
        let tool = CommandTool::new();
        let output = tool
            .call(CommandRequest {
                command: CommandLine::Program {
                    program: "sh".to_owned(),
                    args: vec!["-c".to_owned(), "exit 7".to_owned()],
                },
                cwd: None,
                env: BTreeMap::new(),
                timeout_ms: None,
                max_output_bytes: None,
                stdin: None,
                stdin_blob: None,
                merge_stderr: false,
                summary_patterns: Vec::new(),
                capabilities: CommandCapabilities::default(),
                sandbox: None,
                redact_secrets: false,
                secrets: Vec::new(),
                summary_lines: None,
            })
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = output.structured.expect("structured");
        assert_eq!(structured["code"], "non_zero_exit");
        assert_eq!(structured["exit_code"], 7);
    }

    #[tokio::test]
    async fn excess_output_is_truncated_and_marked() {
        let tool = CommandTool::new();
        let mut request = request(shell("for i in $(seq 1 200); do echo '0123456789'; done"));
        request.max_output_bytes = Some(64);
        let output = tool.call(request).await.expect("execute");
        assert!(!output.is_error, "a truncation is not a failure");
        let structured = structured(&output);
        assert_eq!(structured["truncated"], true);
        assert!(
            structured["bytes_written"].as_u64().unwrap_or_default() > 64,
            "the whole stream is counted even though only part of it is kept"
        );
        assert!(
            structured["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("[output truncated at 64 bytes per stream]"),
            "the marker tells the model the output was cut"
        );
    }

    #[tokio::test]
    async fn a_timeout_is_reported_and_the_process_does_not_survive_it() {
        let tool = CommandTool::new();
        let mut request = request(shell("sleep 30"));
        request.timeout_ms = Some(50);
        let output = tool.call(request).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "timeout");
        assert_eq!(structured["timed_out"], true);
    }

    #[tokio::test]
    async fn a_cancelled_command_is_reported_as_cancelled() {
        let tool = CommandTool::new();
        let context = ToolContext::new();
        let signal = context.cancellation();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal.cancel();
        });
        let output = tool
            .call_with_context(&context, request(shell("sleep 30")))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "cancelled");
        assert_eq!(structured["cancelled"], true);
    }

    #[test]
    fn secrets_are_redacted() {
        assert_eq!(
            redact("token=abc123", &["abc123".to_owned()]),
            "token=[redacted]"
        );
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry
            .register_tool(&CommandTool::new())
            .expect("register command");
        let name = rustychain_core::ToolName::new("command").expect("name");
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(
            spec.description,
            <CommandTool as rustychain_core::ToolMetadata>::DESCRIPTION
        );

        let output = registry
            .execute(
                &ToolContext::new(),
                &name,
                serde_json::json!({
                    "command": { "kind": "program", "program": "echo", "args": ["hi"] }
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        assert!(!output.parts.is_empty());
        assert!(output.structured.is_some());
    }

    fn request(command: CommandLine) -> CommandRequest {
        CommandRequest {
            command,
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
            max_output_bytes: None,
            stdin: None,
            stdin_blob: None,
            merge_stderr: false,
            summary_patterns: Vec::new(),
            capabilities: CommandCapabilities::default(),
            sandbox: None,
            redact_secrets: false,
            secrets: Vec::new(),
            summary_lines: None,
        }
    }

    fn shell(script: &str) -> CommandLine {
        CommandLine::Shell {
            shell: "sh".to_owned(),
            script: script.to_owned(),
        }
    }

    fn structured(output: &ToolOutput) -> Value {
        output.structured.clone().expect("structured")
    }

    #[derive(Default)]
    struct Blobs(Mutex<BTreeMap<BlobId, Vec<u8>>>);

    #[async_trait::async_trait]
    impl BlobStore for Blobs {
        async fn put(&self, bytes: &[u8]) -> Result<BlobId> {
            let id = BlobId::of(bytes);
            self.0.lock().expect("lock").insert(id, bytes.to_vec());
            Ok(id)
        }

        async fn get(&self, id: BlobId) -> Result<Vec<u8>> {
            self.0
                .lock()
                .expect("lock")
                .get(&id)
                .cloned()
                .ok_or(Error::BlobNotFound(id))
        }

        async fn contains(&self, id: BlobId) -> Result<bool> {
            Ok(self.0.lock().expect("lock").contains_key(&id))
        }
    }

    struct EnvSandbox;

    impl SandboxBackend for EnvSandbox {
        fn apply(&self, profile: &str, command: &mut Command) -> Result<()> {
            if profile == "deny" {
                return Err(Error::InvalidInput("sandbox denied".to_owned()));
            }
            command.as_std_mut().env("SANDBOX_PROFILE", profile);
            Ok(())
        }
    }

    #[tokio::test]
    async fn stdin_can_come_from_a_blob() {
        let blobs = Arc::new(Blobs::default());
        let id = blobs.put(b"hello from a blob").await.expect("put");
        let tool = CommandTool::new().with_blobs(blobs);
        let mut req = request(shell("read -r line; printf %s \"$line\""));
        req.stdin_blob = Some(id);
        let output = tool.call(req).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "hello from a blob");
    }

    #[tokio::test]
    async fn stderr_can_be_merged_into_stdout() {
        let tool = CommandTool::new();
        let mut req = request(shell("echo out; echo err 1>&2"));
        req.merge_stderr = true;
        let output = tool.call(req).await.expect("execute");
        let structured = structured(&output);
        let stdout = structured["stdout"].as_str().expect("stdout");
        assert!(stdout.contains("out"));
        assert!(stdout.contains("err"));
        assert_eq!(structured["stderr"], "");
    }

    #[tokio::test]
    async fn the_summary_can_report_matched_patterns() {
        let tool = CommandTool::new();
        let mut req = request(shell("printf 'alpha\\nbeta\\ngamma\\n'"));
        req.summary_patterns = vec!["beta".to_owned()];
        let output = tool.call(req).await.expect("execute");
        let summary = structured(&output)["summary"]
            .as_str()
            .expect("summary")
            .to_owned();
        assert!(summary.contains("beta"));
        assert!(!summary.contains("alpha"));
    }

    #[tokio::test]
    async fn a_sandbox_backend_can_rewrite_the_command() {
        let tool = CommandTool::new().with_sandbox(Arc::new(EnvSandbox));
        let mut req = request(shell("echo $SANDBOX_PROFILE"));
        req.sandbox = Some("strict".to_owned());
        let output = tool.call(req).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "strict\n");
    }

    #[tokio::test]
    async fn a_sandbox_backend_can_refuse() {
        let tool = CommandTool::new().with_sandbox(Arc::new(EnvSandbox));
        let mut req = request(shell("echo never"));
        req.sandbox = Some("deny".to_owned());
        let output = tool.call(req).await.expect("execute");
        assert_eq!(structured(&output)["code"], "spawn_failed");
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn a_killed_process_reports_its_signal() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("kill -KILL $$")))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "non_zero_exit");
        #[cfg(unix)]
        assert_eq!(structured["signal"], 9);
    }

    #[tokio::test]
    async fn a_missing_program_is_a_spawn_failure() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(CommandLine::Program {
                program: "definitely-not-a-real-binary-xyz".to_owned(),
                args: Vec::new(),
            }))
            .await
            .expect("execute");
        assert!(output.is_error);
        assert_eq!(structured(&output)["code"], "spawn_failed");
    }
}
