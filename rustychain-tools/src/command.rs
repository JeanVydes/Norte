//! The flagship tool of the suite: a bounded command executor.
//!
//! The executor runs one program or one shell script, streams what it writes,
//! keeps that output bounded, and reports a structured outcome (FR-TOOL-BASH-1
//! to FR-TOOL-BASH-8). Four properties shape the code:
//!
//! * **The host bounds; the request may only ask for less.** Timeouts, output
//!   caps and the size of the text that reaches the model come from
//!   [`CommandLimits`]; a request that asks for more is clamped, never obeyed.
//! * **The host decides; the request cannot widen.** The process starts from the
//!   host's environment and cannot override a variable the host set, and the
//!   secrets the host declares are redacted from output, from the live stream
//!   and from the audit record whether the request likes it or not.
//! * **The process group does not outlive the call.** On timeout, on
//!   cancellation, when a pipe will not close and when the future is dropped,
//!   the whole group is killed.
//! * **Authorization is not the executor's business.** A request can name any
//!   program, so deciding which commands may run belongs to the host's approval
//!   ledger and policies (FR-APPR-1, FR-POL-1). A sandbox is a lower-level
//!   abstraction than running a tool, so the suite ships none.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
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

/// The shell [`CommandRequest::shell`] runs with unless it names another one.
pub const DEFAULT_SHELL: &str = "sh";

/// What a redacted secret becomes in output, progress and the audit record.
const REDACTED: &[u8] = b"[redacted]";

/// How much one read from a pipe asks for.
const READ_BUFFER: usize = 8192;

/// Program invocation. Exactly one of a direct program or a shell script.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "kind", deny_unknown_fields)]
#[schemars(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandLine {
    /// Run a program with its arguments, with no shell in between.
    #[schemars(
        example = serde_json::json!({"kind": "program", "program": "cargo", "args": ["test"]})
    )]
    Program {
        #[schemars(description = "The program to run.", example = &"cargo")]
        program: String,
        #[serde(default)]
        #[schemars(description = "The program's arguments, in order.")]
        args: Vec<String>,
    },
    /// Run a script through `shell -c`.
    #[schemars(
        description = "Run a script with the shell's -c flag.",
        example = serde_json::json!({"kind": "shell", "shell": "sh", "script": "echo $HOME"})
    )]
    Shell {
        /// The interpreter; [`CommandRequest::shell`] fills in [`DEFAULT_SHELL`].
        #[schemars(description = "The shell to run the script with.", example = &"sh")]
        shell: String,
        #[schemars(description = "The script to run.")]
        script: String,
    },
}

/// What to run and how. Everything here is a *request*: each bound is clamped
/// against the host's [`CommandLimits`], and no field can widen what the host
/// allows (FR-TOOL-BASH-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
#[schemars(
    description = "The command to run and the bounds for it. The host caps the run time and the output size, so asking for more than it allows is clamped, not honoured."
)]
pub struct CommandRequest {
    pub command: CommandLine,
    /// Working directory for the process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(example = &"/srv/app")]
    pub cwd: Option<PathBuf>,
    /// Variables to add to the process. The process does not inherit anything
    /// else, and a variable the host set cannot be overridden here
    /// (FR-TOOL-BASH-6).
    #[schemars(
        description = "Extra environment variables for the process. The process inherits nothing else, and a variable the host already set cannot be replaced here."
    )]
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// How long the process may run, in milliseconds. A value above the host's
    /// ceiling is clamped to it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// How many bytes of each stream the executor keeps. A value above the
    /// host's ceiling is clamped to it; the model sees at most
    /// [`CommandLimits::max_model_bytes`] of it.
    #[schemars(
        description = "How many bytes of each stream to keep. Above the host's ceiling it is clamped to it, and what reaches you is capped further by the host."
    )]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_bytes: Option<usize>,
    /// Bytes for the process's standard input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    /// A blob holding standard input; takes precedence over `stdin`
    /// (FR-TOOL-BASH-1).
    #[schemars(description = "A blob holding the standard input; it wins over the inline stdin.")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_blob: Option<BlobId>,
    /// Merge stderr into stdout for both the live stream and the final text.
    #[serde(default)]
    pub merge_stderr: bool,
    /// Lines to include in the structured summary when they contain any of
    /// these literal patterns (FR-TOOL-BASH-3).
    #[schemars(
        description = "Report the lines that contain any of these literal strings in the structured summary."
    )]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub summary_patterns: Vec<String>,
    /// The last N lines of the output to include in the structured summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_lines: Option<usize>,
}

impl CommandRequest {
    fn bare(command: CommandLine) -> Self {
        Self {
            command,
            cwd: None,
            env: BTreeMap::new(),
            timeout_ms: None,
            max_output_bytes: None,
            stdin: None,
            stdin_blob: None,
            merge_stderr: false,
            summary_patterns: Vec::new(),
            summary_lines: None,
        }
    }

    /// A request that runs `program` with no arguments.
    #[must_use]
    pub fn program(program: impl Into<String>) -> Self {
        Self::bare(CommandLine::Program {
            program: program.into(),
            args: Vec::new(),
        })
    }

    /// A request that runs `script` through [`DEFAULT_SHELL`].
    #[must_use]
    pub fn shell(script: impl Into<String>) -> Self {
        Self::bare(CommandLine::Shell {
            shell: DEFAULT_SHELL.to_owned(),
            script: script.into(),
        })
    }

    /// Adds arguments to a [`CommandLine::Program`]; ignored otherwise.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        if let CommandLine::Program { args: current, .. } = &mut self.command {
            current.extend(args.into_iter().map(Into::into));
        }
        self
    }

    /// Runs the script through another shell; ignored for a program.
    #[must_use]
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        if let CommandLine::Shell { shell: current, .. } = &mut self.command {
            *current = shell.into();
        }
        self
    }

    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        self
    }

    #[must_use]
    pub fn with_output_cap(mut self, bytes: usize) -> Self {
        self.max_output_bytes = Some(bytes);
        self
    }

    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<String>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    #[must_use]
    pub fn with_stdin_blob(mut self, blob: BlobId) -> Self {
        self.stdin_blob = Some(blob);
        self
    }

    #[must_use]
    pub fn merging_stderr(mut self) -> Self {
        self.merge_stderr = true;
        self
    }

    #[must_use]
    pub fn with_summary_lines(mut self, lines: usize) -> Self {
        self.summary_lines = Some(lines);
        self
    }

    #[must_use]
    pub fn with_summary_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.summary_patterns.push(pattern.into());
        self
    }
}

/// How an invocation ended. The distinction is what tells a model whether to
/// retry, to change the command, or to give up (FR-TOOL-BASH-4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandCode {
    /// The process exited zero and its output was read whole.
    Success,
    /// The process exited non-zero, of its own accord.
    NonZeroExit,
    /// The process died from a signal the executor did not send.
    Signaled,
    /// The host's time bound was reached and the process group was killed.
    Timeout,
    /// The caller cancelled and the process group was killed.
    Cancelled,
    /// The request itself could not be served (an unresolvable stdin blob).
    InvalidInput,
    /// The process never started.
    SpawnFailed,
    /// The executor could not spawn, wait for, or read the process.
    IoError,
}

/// The structured outcome of one invocation (FR-TOOL-BASH-3): what the process
/// wrote, how it ended, and the digests that make it auditable
/// (FR-TOOL-BASH-8).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutput {
    pub code: CommandCode,
    pub exit_code: Option<i32>,
    /// The signal that ended the process, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Standard output, redacted and bounded. When the executor had to cut
    /// something, this text carries a marker saying so.
    pub stdout: String,
    /// Standard error, redacted and bounded; empty when the request merged it.
    pub stderr: String,
    pub duration_ms: u64,
    /// Bytes the process wrote in total, across both streams, whether or not
    /// they were kept.
    pub bytes_read: usize,
    /// Whether any of what the process wrote was left out of this result.
    pub truncated: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    /// The directory the process ran in, when the request named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// The optional machine-readable summary (FR-TOOL-BASH-3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Why the *executor* failed, when it did: a spawn that did not happen, a
    /// wait that errored, a pipe that broke. The process's own diagnostics are
    /// in `stderr`, never here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Digest of the request as the executor judged it, with any declared
    /// secret already redacted.
    pub request_digest: Digest,
    /// Digest of the text this result carries.
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
            stderr: String::new(),
            duration_ms: u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            bytes_read: 0,
            truncated: false,
            timed_out: false,
            cancelled: false,
            cwd: None,
            summary: None,
            reason: Some(reason.into()),
            request_digest,
            output_hash: Digest::of(&[]),
            started_at,
            ended_at: Utc::now(),
        }
    }
}

/// Packs a [`CommandOutput`] into the text and structure the model receives.
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
        let _ = writeln!(rendered, "bytes_read: {}", output.bytes_read);
        if output.truncated {
            rendered.push_str("truncated: true\n");
        }
        if let Some(cwd) = &output.cwd {
            let _ = writeln!(rendered, "cwd: {}", cwd.display());
        }
        if let Some(reason) = &output.reason {
            let _ = writeln!(rendered, "reason: {reason}");
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

/// The host's bounds for every invocation: the executor's answers to "how long
/// may this run", "how much may it write" and "how long do I wait for the pipes
/// once it is gone".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandLimits {
    /// Used when the request does not ask for a time bound.
    pub default_timeout: Duration,
    /// Ceiling for a request's `timeout_ms`.
    pub max_timeout: Duration,
    /// How long a signalled process group has to leave before it is killed.
    pub grace: Duration,
    /// How long to wait for the output pipes once the process has been reaped.
    /// A process that leaves a child holding them cannot stall the call.
    pub drain: Duration,
    /// Bytes kept per stream, and the ceiling for a request's
    /// `max_output_bytes`.
    pub max_output_bytes: usize,
    /// Bytes of each stream (and of the summary) that reach the model; what is
    /// left out is elided around both ends with a marker.
    pub max_model_bytes: usize,
    /// Bytes of the *last* output kept for the summary's tail, whatever
    /// [`Self::max_output_bytes`] kept at the front.
    pub tail_bytes: usize,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
            grace: Duration::from_secs(2),
            drain: Duration::from_secs(2),
            max_output_bytes: 1024 * 1024,
            max_model_bytes: 32 * 1024,
            tail_bytes: 64 * 1024,
        }
    }
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
    env: BTreeMap<String, String>,
    secrets: Arc<[String]>,
}

impl CommandTool {
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: CommandLimits::default(),
            renderer: Arc::new(PlainCommandRenderer),
            blobs: None,
            env: BTreeMap::new(),
            secrets: Arc::from(Vec::new()),
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

    /// Supplies the blob store, so a `stdin_blob` can be resolved
    /// (FR-TOOL-BASH-1).
    #[must_use]
    pub fn with_blobs(mut self, blobs: Arc<dyn BlobStore>) -> Self {
        self.blobs = Some(blobs);
        self
    }

    /// The environment every process starts from. A request may add variables
    /// but cannot replace one of these (FR-TOOL-BASH-6).
    #[must_use]
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    /// The values to redact. Redaction is always on: a secret declared here is
    /// replaced in the model's text, in the live stream and in the audit
    /// record, whatever the request asks for (FR-TOOL-BASH-6). Longer secrets
    /// are redacted first, so overlapping ones cannot leave a tail behind.
    #[must_use]
    pub fn with_secrets(mut self, secrets: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut secrets: Vec<String> = secrets.into_iter().map(Into::into).collect();
        secrets.retain(|secret| !secret.is_empty());
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        self.secrets = Arc::from(secrets);
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<CommandRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    /// The time bound for this request: what it asked for, clamped to the
    /// host's ceiling, or the host's default.
    fn timeout(&self, request: &CommandRequest) -> Duration {
        request
            .timeout_ms
            .map_or(self.limits.default_timeout, Duration::from_millis)
            .min(self.limits.max_timeout)
    }

    /// The capture cap for this request, clamped to the host's ceiling.
    fn output_cap(&self, request: &CommandRequest) -> usize {
        request
            .max_output_bytes
            .unwrap_or(self.limits.max_output_bytes)
            .min(self.limits.max_output_bytes)
    }

    /// The request with every declared secret replaced, for the audit digest:
    /// a digest of the raw request would leak a secret to anyone who can guess
    /// the rest of it.
    fn digest(&self, request: &CommandRequest) -> Result<Digest> {
        let mut value = serde_json::to_value(request)?;
        redact_value(&mut value, &self.secrets);
        Ok(Digest::of_json(&value))
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

    fn build_command(&self, request: &CommandRequest) -> Command {
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
        for (key, value) in &self.env {
            command.env(key, value);
        }
        for (key, value) in &request.env {
            if !self.env.contains_key(key) {
                command.env(key, value);
            }
        }
        if let Some(cwd) = &request.cwd {
            command.current_dir(cwd);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command.kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        command
    }

    fn report(&self, output: &CommandOutput, command_line: &str) -> Result<ToolOutput> {
        audit(output, command_line);
        render(&*self.renderer, output)
    }

    async fn run(&self, context: &ToolContext, request: &CommandRequest) -> Result<ToolOutput> {
        let clock = Clock::start(self.digest(request)?);
        let command_line = redact(&describe_command(&request.command), &self.secrets);

        if context.is_cancelled() {
            let output = clock.failed(
                CommandCode::Cancelled,
                "cancelled before the process started",
            );
            return self.report(&output, &command_line);
        }

        let stdin = match self.stdin(request).await {
            Ok(bytes) => bytes,
            Err(error) => {
                let code = stdin_code(&error);
                let output = clock.failed(code, error.to_string());
                return self.report(&output, &command_line);
            }
        };

        let mut child = match self.build_command(request).spawn() {
            Ok(child) => child,
            Err(error) => {
                let reason = format!("could not start `{command_line}`: {error}");
                let output = clock.failed(CommandCode::SpawnFailed, reason);
                return self.report(&output, &command_line);
            }
        };

        if let Some(mut pipe) = child.stdin.take() {
            tokio::spawn(async move {
                let _ = pipe.write_all(&stdin).await;
                let _ = pipe.shutdown().await;
            });
        }

        self.execute(context, request, child, &clock, &command_line)
            .await
    }

    async fn execute(
        &self,
        context: &ToolContext,
        request: &CommandRequest,
        mut child: tokio::process::Child,
        clock: &Clock,
        command_line: &str,
    ) -> Result<ToolOutput> {
        let pid = child.id();
        let mut guard = GroupGuard::new(pid);
        let (streams, cap) = self.start_readers(context, request, &mut child);

        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            status = child.wait() => status,
            () = tokio::time::sleep(self.timeout(request)) => {
                timed_out = true;
                self.terminate(&mut child, &mut guard).await
            }
            () = context.cancelled() => {
                cancelled = true;
                self.terminate(&mut child, &mut guard).await
            }
        };

        let drained = streams
            .drain(
                self.limits.drain,
                context,
                request.merge_stderr,
                pid,
                &mut child,
                &mut guard,
            )
            .await;

        let read_error = drained
            .stdout
            .error
            .clone()
            .or_else(|| drained.stderr.error.clone());
        let (code, exit_code, signal) =
            outcome(&status, cancelled, timed_out, read_error.is_some());
        let mut reason = wait_reason(&status);
        if let Some(error) = read_error {
            reason = Some(append(
                reason,
                &format!("reading the output failed: {error}"),
            ));
        }
        if !drained.complete {
            reason = Some(append(
                reason,
                "the output pipes were still open after the drain deadline; output may be incomplete",
            ));
        }

        let decoded = decode(&Decode {
            request,
            stdout: &drained.stdout,
            stderr: &drained.stderr,
            merged: drained.merged.as_ref(),
            cap,
            budget: self.limits.max_model_bytes,
        });
        let mut output_hash = decoded.stdout.as_bytes().to_vec();
        output_hash.extend_from_slice(decoded.stderr.as_bytes());

        let output = CommandOutput {
            code,
            exit_code,
            signal,
            stdout: decoded.stdout,
            stderr: decoded.stderr,
            duration_ms: clock.elapsed_ms(),
            bytes_read: decoded.bytes_read,
            truncated: decoded.truncated,
            timed_out,
            cancelled,
            cwd: request.cwd.clone(),
            summary: decoded.summary,
            reason,
            request_digest: clock.digest,
            output_hash: Digest::of(&output_hash),
            started_at: clock.started_at,
            ended_at: Utc::now(),
        };
        self.report(&output, command_line)
    }

    /// Starts one reader per pipe. Each redacts before it keeps anything, so no
    /// secret can reach the capture, the live stream or the audit record.
    fn start_readers(
        &self,
        context: &ToolContext,
        request: &CommandRequest,
        child: &mut tokio::process::Child,
    ) -> (Streams, usize) {
        let cap = self.output_cap(request);
        let capture = Arc::new(Mutex::new(Captured::new(cap, self.limits.tail_bytes)));
        let errors = Arc::new(Mutex::new(Captured::new(cap, self.limits.tail_bytes)));
        let merged = request
            .merge_stderr
            .then(|| Arc::new(Mutex::new(SharedCapture::new(cap, self.limits.tail_bytes))));
        let merge = request.merge_stderr;

        let stdout_task = tokio::spawn(
            StreamReader::new(
                StreamRole::Stdout,
                context,
                &self.secrets,
                cap,
                merge,
                capture.clone(),
                merged.clone(),
            )
            .run(child.stdout.take()),
        );
        let stderr_task = tokio::spawn(
            StreamReader::new(
                StreamRole::Stderr,
                context,
                &self.secrets,
                cap,
                merge,
                errors.clone(),
                merged.clone(),
            )
            .run(child.stderr.take()),
        );
        let streams = Streams {
            stdout: capture,
            stderr: errors,
            merged,
            stdout_task,
            stderr_task,
        };
        (streams, cap)
    }

    async fn terminate(
        &self,
        child: &mut tokio::process::Child,
        guard: &mut GroupGuard,
    ) -> std::io::Result<ExitStatus> {
        let pid = child.id();
        kill_group(pid, Signal::Terminate, child);
        if let Ok(status) = with_timeout(self.limits.grace, child.wait()).await {
            return status;
        }
        kill_group(pid, Signal::Kill, child);
        guard.disarm();
        with_timeout(self.limits.grace, child.wait())
            .await
            .unwrap_or_else(|_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the process did not die after SIGKILL",
                ))
            })
    }
}

/// The two readers of one invocation, and where they file what they read.
struct Streams {
    stdout: Arc<Mutex<Captured>>,
    stderr: Arc<Mutex<Captured>>,
    merged: Option<Arc<Mutex<SharedCapture>>>,
    stdout_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
}

/// What the readers left behind.
struct Drained {
    stdout: Captured,
    stderr: Captured,
    merged: Option<SharedCapture>,
    /// Whether both readers ended on their own before the deadline.
    complete: bool,
}

impl Streams {
    /// Waits for the readers, but not forever: a process that hands its pipes
    /// to a child and exits would otherwise stall the call for as long as that
    /// child lives. Past the deadline the group is killed and whatever arrived
    /// is taken.
    async fn drain(
        self,
        deadline: Duration,
        context: &ToolContext,
        merge: bool,
        pid: Option<u32>,
        child: &mut tokio::process::Child,
        guard: &mut GroupGuard,
    ) -> Drained {
        let stdout_abort = self.stdout_task.abort_handle();
        let stderr_abort = self.stderr_task.abort_handle();
        let complete = with_timeout(deadline, async {
            let _ = self.stdout_task.await;
            let _ = self.stderr_task.await;
        })
        .await
        .is_ok();

        if !complete {
            kill_group(pid, Signal::Kill, child);
            stdout_abort.abort();
            stderr_abort.abort();
            close_abandoned(context, merge, &self.stdout, &self.stderr);
        }
        guard.disarm();

        Drained {
            stdout: self.stdout.lock().expect("command stdout").clone(),
            stderr: self.stderr.lock().expect("command stderr").clone(),
            merged: self
                .merged
                .map(|shared| shared.lock().expect("command merged").clone()),
            complete,
        }
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

/// The invocation as one line, for the audit record. It is redacted before it
/// is logged, because an argument is often where a secret lives.
fn describe_command(command: &CommandLine) -> String {
    match command {
        CommandLine::Program { program, args } => {
            let mut line = program.clone();
            for arg in args {
                line.push(' ');
                line.push_str(arg);
            }
            line
        }
        CommandLine::Shell { shell, script } => format!("{shell} -c {script}"),
    }
}

/// Where a request's start time and identity came from, so a failure that
/// happens before anything runs still reports an auditable record.
struct Clock {
    started: Instant,
    started_at: DateTime<Utc>,
    digest: Digest,
}

impl Clock {
    fn start(digest: Digest) -> Self {
        Self {
            started: Instant::now(),
            started_at: Utc::now(),
            digest,
        }
    }

    fn failed(&self, code: CommandCode, reason: impl Into<String>) -> CommandOutput {
        CommandOutput::failed(
            code,
            reason,
            self.started.elapsed(),
            self.digest,
            self.started_at,
        )
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// The head of what a stream wrote, the rolling tail kept for the summary, and
/// how much the process wrote in total.
#[derive(Clone, Debug)]
struct Captured {
    cap: usize,
    head: Vec<u8>,
    tail: RollingTail,
    total: usize,
    /// Whether anything the process wrote was left out of `head`.
    dropped: bool,
    /// Whether the reader ran to completion, including its closing marker.
    finished: bool,
    /// The read error that ended the stream, when one did.
    error: Option<String>,
}

impl Captured {
    fn new(cap: usize, tail_bytes: usize) -> Self {
        Self {
            cap,
            head: Vec::new(),
            tail: RollingTail::new(tail_bytes),
            total: 0,
            dropped: false,
            finished: false,
            error: None,
        }
    }

    /// Adds what one read produced, redacted, keeping at most `cap` bytes at
    /// the front and the last `tail_bytes` for the summary.
    fn push(&mut self, clean: &[u8]) {
        let room = self.cap.saturating_sub(self.head.len());
        if clean.len() > room {
            self.dropped = true;
        }
        let take = clean.len().min(room);
        self.head.extend_from_slice(&clean[..take]);
        self.tail.push(clean);
    }

    /// The text the summary reads: the whole head when nothing was cut, its
    /// head plus the rolling tail otherwise.
    fn retained(&self) -> String {
        let head = lossy(&self.head);
        if self.dropped {
            format!("{head}{}", self.tail.text())
        } else {
            head
        }
    }

    /// The last of what the process wrote, whatever the cap kept.
    fn last(&self) -> String {
        if self.dropped {
            self.tail.text()
        } else {
            lossy(&self.head)
        }
    }
}

/// The merged stream: both pipes, in the order the readers saw them.
#[derive(Clone, Debug)]
struct SharedCapture {
    cap: usize,
    bytes: Vec<u8>,
    tail: RollingTail,
    dropped: bool,
}

impl SharedCapture {
    fn new(cap: usize, tail_bytes: usize) -> Self {
        Self {
            cap,
            bytes: Vec::new(),
            tail: RollingTail::new(tail_bytes),
            dropped: false,
        }
    }

    fn push(&mut self, clean: &[u8]) {
        let room = self.cap.saturating_sub(self.bytes.len());
        if clean.len() > room {
            self.dropped = true;
        }
        let take = clean.len().min(room);
        self.bytes.extend_from_slice(&clean[..take]);
        self.tail.push(clean);
    }

    fn text(&self) -> String {
        lossy(&self.bytes)
    }

    fn retained(&self) -> String {
        let head = self.text();
        if self.dropped {
            format!("{head}{}", self.tail.text())
        } else {
            head
        }
    }

    fn last(&self) -> String {
        if self.dropped {
            self.tail.text()
        } else {
            self.text()
        }
    }
}

/// The last N bytes of a stream, kept for the summary even when the capture cap
/// has already filled up at the front.
#[derive(Clone, Debug)]
struct RollingTail {
    budget: usize,
    bytes: Vec<u8>,
}

impl RollingTail {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: Vec::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.budget {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.budget..]);
            return;
        }
        let overflow = self
            .bytes
            .len()
            .saturating_add(chunk.len())
            .saturating_sub(self.budget);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend_from_slice(chunk);
    }

    fn text(&self) -> String {
        lossy(&self.bytes)
    }
}

/// Which pipe a reader owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StreamRole {
    Stdout,
    Stderr,
}

impl StreamRole {
    fn stream(self) -> ToolStream {
        match self {
            Self::Stdout => ToolStream::Stdout,
            Self::Stderr => ToolStream::Stderr,
        }
    }
}

/// One reader per pipe: it redacts what it reads before any of it is kept,
/// streamed or counted, so a secret cannot slip out through a cap boundary, a
/// progress update or a truncation marker (FR-TOOL-BASH-6).
struct StreamReader {
    role: StreamRole,
    context: ToolContext,
    secrets: Arc<[String]>,
    cap: usize,
    merge: bool,
    capture: Arc<Mutex<Captured>>,
    shared: Option<Arc<Mutex<SharedCapture>>>,
}

impl StreamReader {
    fn new(
        role: StreamRole,
        context: &ToolContext,
        secrets: &Arc<[String]>,
        cap: usize,
        merge: bool,
        capture: Arc<Mutex<Captured>>,
        shared: Option<Arc<Mutex<SharedCapture>>>,
    ) -> Self {
        Self {
            role,
            context: context.clone(),
            secrets: secrets.clone(),
            cap,
            merge,
            capture,
            shared,
        }
    }

    /// The stream progress and the closing marker go to.
    fn emitted(&self) -> ToolStream {
        if self.merge {
            ToolStream::Stdout
        } else {
            self.role.stream()
        }
    }

    async fn run<R>(self, pipe: Option<R>)
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let Some(mut pipe) = pipe else {
            self.close();
            return;
        };
        let mut redactor = Redactor::new(self.secrets.clone());
        let mut buffer = vec![0u8; READ_BUFFER];
        let mut sent = 0usize;
        loop {
            match pipe.read(&mut buffer).await {
                Ok(0) => break,
                Ok(read) => {
                    self.capture.lock().expect("command capture").total += read;
                    let clean = redactor.feed(&buffer[..read]);
                    self.keep(&clean, &mut sent);
                }
                Err(error) => {
                    self.capture.lock().expect("command capture").error = Some(error.to_string());
                    break;
                }
            }
        }
        let rest = redactor.finish();
        self.keep(&rest, &mut sent);
        self.close();
    }

    /// Files what came out of the redactor, under the cap, and streams it on
    /// until the cap has been reached.
    fn keep(&self, clean: &[u8], sent: &mut usize) {
        if clean.is_empty() {
            return;
        }
        self.capture.lock().expect("command capture").push(clean);
        if let Some(shared) = &self.shared {
            shared.lock().expect("command merged").push(clean);
        }
        let budget = self.cap.saturating_sub(*sent);
        if budget > 0 {
            let take = budget.min(clean.len());
            let progress = ToolProgress {
                stream: self.emitted(),
                bytes: clean[..take].to_vec(),
                last: false,
            };
            self.context.emit(progress);
            *sent += take;
        }
    }

    fn close(&self) {
        self.capture.lock().expect("command capture").finished = true;
        if !self.merge || self.role == StreamRole::Stdout {
            let progress = ToolProgress {
                stream: self.emitted(),
                bytes: Vec::new(),
                last: true,
            };
            self.context.emit(progress);
        }
    }
}

/// Holds back the tail of what it is given until no secret can still be
/// completed by a later byte, so redaction never depends on where a read
/// happened to split the stream.
struct Redactor {
    secrets: Arc<[String]>,
    hold: Vec<u8>,
    /// One less than the longest secret: the most bytes of a secret that can
    /// still be outstanding across a chunk boundary.
    hold_limit: usize,
}

impl Redactor {
    fn new(secrets: Arc<[String]>) -> Self {
        let hold_limit = secrets
            .iter()
            .map(String::len)
            .max()
            .map_or(0, |longest| longest.saturating_sub(1));
        Self {
            secrets,
            hold: Vec::new(),
            hold_limit,
        }
    }

    /// Returns everything that is safe to hand on: old enough that no secret
    /// starting inside it can still be completed by the next chunk.
    fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut buffer = std::mem::take(&mut self.hold);
        buffer.extend_from_slice(chunk);
        let mut clean = redact_bytes(&buffer, &self.secrets);
        let keep = self.hold_limit.min(clean.len());
        let emit = clean.len() - keep;
        self.hold = clean.split_off(emit);
        clean
    }

    /// The pending tail, redacted, at end of stream.
    fn finish(&mut self) -> Vec<u8> {
        let mut clean = redact_bytes(&std::mem::take(&mut self.hold), &self.secrets);
        self.hold.clear();
        std::mem::take(&mut clean)
    }
}

/// What one result carries, after the streams are read.
struct Decoded {
    stdout: String,
    stderr: String,
    truncated: bool,
    summary: Option<String>,
    bytes_read: usize,
}

/// The inputs of [`decode`].
struct Decode<'a> {
    request: &'a CommandRequest,
    stdout: &'a Captured,
    stderr: &'a Captured,
    /// The merged streams, when the request asked for them.
    merged: Option<&'a SharedCapture>,
    cap: usize,
    budget: usize,
}

/// Turns what was read into what the model sees: the retained text, the
/// truncation markers, the structured summary, and the bounded model text.
fn decode(input: &Decode<'_>) -> Decoded {
    let mut truncated = input.stdout.dropped || input.stderr.dropped;
    let bytes_read = input.stdout.total + input.stderr.total;

    let (stdout, stderr) = if let Some(merged) = input.merged {
        truncated |= merged.dropped;
        let mut stdout = merged.text();
        if merged.dropped {
            stdout.push_str(&cap_marker(input.cap, "output"));
        }
        (stdout, String::new())
    } else {
        let mut stdout = lossy(&input.stdout.head);
        let mut stderr = lossy(&input.stderr.head);
        if input.stdout.dropped {
            stdout.push_str(&cap_marker(input.cap, "stdout"));
        }
        if input.stderr.dropped {
            stderr.push_str(&cap_marker(input.cap, "stderr"));
        }
        (stdout, stderr)
    };

    // The summary reads the retained output, not the capped head: the tail of
    // what the process wrote is the useful part.
    let (tail, scan) = build_views(input);
    let summary = build_summary(input.request, &tail, &scan, input.budget);

    let (stdout, stdout_cut) = clip(&stdout, input.budget);
    let (stderr, stderr_cut) = clip(&stderr, input.budget);
    truncated |= stdout_cut || stderr_cut;

    Decoded {
        stdout,
        stderr,
        truncated,
        summary,
        bytes_read,
    }
}

/// What the summary reads: the last of the output for its tail, and the
/// retained text to match patterns against.
fn build_views(input: &Decode<'_>) -> (String, Vec<String>) {
    if let Some(merged) = input.merged {
        return (merged.last(), vec![merged.retained()]);
    }
    let tail = format!("{}{}", input.stdout.last(), input.stderr.last());
    (tail, vec![input.stdout.retained(), input.stderr.retained()])
}

/// The optional structured summary: the tail of the output and/or the lines
/// that match any host-supplied literal pattern (FR-TOOL-BASH-3).
fn build_summary(
    request: &CommandRequest,
    tail: &str,
    scan: &[String],
    budget: usize,
) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    if let Some(lines) = request.summary_lines {
        sections.push(format!("tail:\n{}", tail_lines(tail, lines)));
    }
    if !request.summary_patterns.is_empty() {
        sections.push(format!(
            "matches:\n{}",
            matching_lines(scan, &request.summary_patterns)
        ));
    }
    if sections.is_empty() {
        return None;
    }
    Some(clip(&sections.join("\n"), budget).0)
}

fn matching_lines(texts: &[String], patterns: &[String]) -> String {
    let mut found: Vec<&str> = Vec::new();
    for text in texts {
        for line in text.lines() {
            let hit = patterns
                .iter()
                .any(|pattern| !pattern.is_empty() && line.contains(pattern.as_str()));
            if hit {
                found.push(line);
            }
        }
    }
    found.join("\n")
}

/// Keeps at most `budget` bytes of `text`, both ends, with a marker in the
/// middle; returns whether anything was left out.
fn clip(text: &str, budget: usize) -> (String, bool) {
    if text.len() <= budget {
        return (text.to_owned(), false);
    }
    let half = budget / 2;
    let head_end = floor_boundary(text, half);
    let tail_start = ceil_boundary(text, text.len() - half);
    let elided = text.len() - head_end - (text.len() - tail_start);
    let clipped = format!(
        "{}\n[… {elided} bytes elided …]\n{}",
        &text[..head_end],
        &text[tail_start..]
    );
    (clipped, true)
}

/// The last index at or below `index` that starts a character.
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// The first index at or above `index` that starts a character.
fn ceil_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

fn cap_marker(cap: usize, stream: &str) -> String {
    format!("[{stream} truncated at {cap} bytes]\n")
}

fn tail_lines(text: &str, lines: usize) -> String {
    let collected: Vec<&str> = text.lines().collect();
    let start = collected.len().saturating_sub(lines);
    collected[start..].join("\n")
}

/// How the process ended, from its status, the two ways the executor can stop
/// it, and whether reading it failed.
fn outcome(
    status: &std::io::Result<ExitStatus>,
    cancelled: bool,
    timed_out: bool,
    read_failed: bool,
) -> (CommandCode, Option<i32>, Option<i32>) {
    let (exit_code, signal) = match status {
        Ok(status) => decompose_status(*status),
        Err(_) => (None, None),
    };
    let code = if cancelled {
        CommandCode::Cancelled
    } else if timed_out {
        CommandCode::Timeout
    } else if read_failed {
        CommandCode::IoError
    } else {
        match status {
            Ok(status) if status.success() => CommandCode::Success,
            Ok(_) if signal.is_some() => CommandCode::Signaled,
            Ok(_) => CommandCode::NonZeroExit,
            Err(_) => CommandCode::IoError,
        }
    };
    (code, exit_code, signal)
}

fn wait_reason(status: &std::io::Result<ExitStatus>) -> Option<String> {
    match status {
        Ok(_) => None,
        Err(error) => Some(format!("waiting for the process failed: {error}")),
    }
}

fn append(reason: Option<String>, note: &str) -> String {
    match reason {
        Some(existing) => format!("{existing}; {note}"),
        None => note.to_owned(),
    }
}

fn stdin_code(error: &Error) -> CommandCode {
    match error {
        Error::BlobNotFound(_) | Error::InvalidInput(_) => CommandCode::InvalidInput,
        _ => CommandCode::IoError,
    }
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

/// The signals the executor sends to a process group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Signal {
    Terminate,
    Kill,
}

/// Signals the whole group, so children the command left behind go with it; a
/// group that is already gone, or a platform without groups, falls back to the
/// process itself.
fn kill_group(pid: Option<u32>, signal: Signal, child: &mut tokio::process::Child) {
    #[cfg(unix)]
    {
        let native = match signal {
            Signal::Terminate => nix::sys::signal::Signal::SIGTERM,
            Signal::Kill => nix::sys::signal::Signal::SIGKILL,
        };
        if let Some(raw) = pid.and_then(|pid| i32::try_from(pid).ok()) {
            let group = nix::unistd::Pid::from_raw(raw);
            if nix::sys::signal::killpg(group, native).is_ok() {
                return;
            }
        }
    }
    #[cfg(not(unix))]
    let _ = signal;
    let _ = child.start_kill();
}

/// Kills the group when the call goes away — a dropped future, an early return
/// — so no child outlives the invocation.
#[cfg(unix)]
#[derive(Debug)]
struct GroupGuard(Option<nix::unistd::Pid>);

#[cfg(unix)]
impl GroupGuard {
    fn new(pid: Option<u32>) -> Self {
        Self(
            pid.and_then(|pid| i32::try_from(pid).ok())
                .map(nix::unistd::Pid::from_raw),
        )
    }

    /// Called once the group has been dealt with, so a recycled process id is
    /// never signalled.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(group) = self.0 {
            let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
#[derive(Debug)]
struct GroupGuard;

#[cfg(not(unix))]
impl GroupGuard {
    fn new(_pid: Option<u32>) -> Self {
        Self
    }

    fn disarm(&mut self) {}
}

/// Closes the streams whose reader was abandoned mid-way, so a consumer waiting
/// for the end of the stream still sees it.
fn close_abandoned(
    context: &ToolContext,
    merge: bool,
    stdout: &Arc<Mutex<Captured>>,
    stderr: &Arc<Mutex<Captured>>,
) {
    let stdout_finished = stdout.lock().expect("command stdout").finished;
    let stderr_finished = stderr.lock().expect("command stderr").finished;
    if !stdout_finished {
        context.emit(ToolProgress {
            stream: ToolStream::Stdout,
            bytes: Vec::new(),
            last: true,
        });
    }
    if !merge && !stderr_finished {
        context.emit(ToolProgress {
            stream: ToolStream::Stderr,
            bytes: Vec::new(),
            last: true,
        });
    }
}

fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Replaces every secret, longest first, in one buffer.
///
/// The secrets are expected longest first, which is how
/// [`CommandTool::with_secrets`] leaves them: replacing a shorter secret first
/// would leave the tail of a longer one in place.
fn redact_bytes(haystack: &[u8], secrets: &[String]) -> Vec<u8> {
    let mut clean = haystack.to_vec();
    for secret in secrets {
        clean = replace_bytes(&clean, secret.as_bytes(), REDACTED);
    }
    clean
}

fn redact(text: &str, secrets: &[String]) -> String {
    lossy(&redact_bytes(text.as_bytes(), secrets))
}

fn redact_value(value: &mut Value, secrets: &[String]) {
    match value {
        Value::String(text) => *text = redact(text, secrets),
        Value::Array(items) => {
            for item in items.iter_mut() {
                redact_value(item, secrets);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_value(item, secrets);
            }
        }
        _ => {}
    }
}

fn replace_bytes(haystack: &[u8], needle: &[u8], with: &[u8]) -> Vec<u8> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return haystack.to_vec();
    }
    let mut out = Vec::with_capacity(haystack.len());
    let mut rest = haystack;
    while let Some(at) = find_bytes(rest, needle) {
        out.extend_from_slice(&rest[..at]);
        out.extend_from_slice(with);
        rest = &rest[at + needle.len()..];
    }
    out.extend_from_slice(rest);
    out
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn audit(output: &CommandOutput, command_line: &str) {
    tracing::info!(
        target: "rustychain.audit",
        command = command_line,
        cwd = ?output.cwd,
        code = ?output.code,
        exit_code = ?output.exit_code,
        signal = ?output.signal,
        duration_ms = output.duration_ms,
        bytes_read = output.bytes_read,
        truncated = output.truncated,
        timed_out = output.timed_out,
        cancelled = output.cancelled,
        reason = ?output.reason,
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
            .call(request(program("echo", &["hello"])))
            .await
            .expect("execute");
        assert!(!output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "success");
        assert_eq!(structured["stdout"], "hello\n");
        assert_eq!(structured["bytes_read"], 6);
        assert!(structured["reason"].is_null());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported_not_raised() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(CommandLine::Program {
                program: "sh".to_owned(),
                args: vec!["-c".to_owned(), "exit 7".to_owned()],
            }))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "non_zero_exit");
        assert_eq!(structured["exit_code"], 7);
    }

    #[tokio::test]
    async fn a_signal_death_is_its_own_code() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("kill -KILL $$")))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "signaled");
        #[cfg(unix)]
        assert_eq!(structured["signal"], 9);
    }

    #[tokio::test]
    async fn a_missing_program_is_a_spawn_failure_that_names_it() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(program("definitely-not-a-real-binary-xyz", &[])))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "spawn_failed");
        assert_eq!(structured["stdout"], "");
        let reason = structured["reason"].as_str().expect("reason");
        assert!(
            reason.contains("definitely-not-a-real-binary-xyz"),
            "the reason names what could not start: {reason}"
        );
    }

    #[tokio::test]
    async fn excess_output_is_truncated_and_marked() {
        let tool = CommandTool::new();
        let request =
            request(shell("for i in $(seq 1 200); do echo '0123456789'; done")).with_output_cap(64);
        let output = tool.call(request).await.expect("execute");
        assert!(!output.is_error, "a truncation is not a failure");
        let structured = structured(&output);
        assert_eq!(structured["truncated"], true);
        assert!(
            structured["bytes_read"].as_u64().unwrap_or_default() > 64,
            "the whole stream is counted even though only part of it is kept"
        );
        assert!(
            structured["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("[stdout truncated at 64 bytes]"),
            "the marker tells the model the output was cut"
        );
    }

    #[tokio::test]
    async fn the_model_text_is_clipped_around_both_ends() {
        let limits = CommandLimits {
            max_model_bytes: 256,
            ..CommandLimits::default()
        };
        let tool = CommandTool::new().with_limits(limits);
        let output = tool
            .call(request(shell(
                "printf 'HEAD\\n'; seq 1 500; printf 'TAIL\\n'",
            )))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert_eq!(structured["truncated"], true);
        let stdout = structured["stdout"].as_str().expect("stdout");
        assert!(stdout.starts_with("HEAD\n"), "the front is kept: {stdout}");
        assert!(stdout.trim_end().ends_with("TAIL"), "the end is kept");
        assert!(
            stdout.contains("bytes elided"),
            "the model is told what it is not seeing"
        );
    }

    #[tokio::test]
    async fn progress_does_not_outrun_the_capture_cap() {
        let tool = CommandTool::new();
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        let request = request(shell("seq 1 500")).with_output_cap(64);
        tool.call_with_context(&context, request)
            .await
            .expect("execute");
        let sent: usize = sink
            .0
            .lock()
            .expect("lock")
            .iter()
            .map(|entry| entry.bytes.len())
            .sum();
        assert!(
            sent <= 64,
            "the live stream is bounded by the same cap the capture uses: {sent}"
        );
    }

    #[tokio::test]
    async fn a_timeout_is_reported_and_the_process_does_not_survive_it() {
        let tool = CommandTool::new();
        let request = request(shell("sleep 30")).with_timeout(Duration::from_millis(50));
        let output = tool.call(request).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "timeout");
        assert_eq!(structured["timed_out"], true);
    }

    #[tokio::test]
    async fn a_request_cannot_extend_the_host_ceiling_on_time() {
        let limits = CommandLimits {
            max_timeout: Duration::from_millis(200),
            grace: Duration::from_millis(50),
            ..CommandLimits::default()
        };
        let tool = CommandTool::new().with_limits(limits);
        let request = request(shell("sleep 30")).with_timeout(Duration::from_secs(600));
        let output = tool.call(request).await.expect("execute");
        let structured = structured(&output);
        assert_eq!(
            structured["code"], "timeout",
            "the request asks for ten minutes; the host allows 200 ms"
        );
    }

    #[tokio::test]
    async fn a_process_that_ignores_the_polite_signal_is_still_killed() {
        let dir = temp_dir("ignores-term");
        let pid_file = dir.join("pid");
        let limits = CommandLimits {
            grace: Duration::from_millis(100),
            ..CommandLimits::default()
        };
        let tool = CommandTool::new().with_limits(limits);
        let script = format!("echo $$ > {}; trap '' TERM; sleep 30", pid_file.display());
        let request = request(shell(&script)).with_timeout(Duration::from_millis(100));
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["code"], "timeout");

        let pid = read_pid(&pid_file);
        #[cfg(unix)]
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
            "the group is killed even though it ignored SIGTERM"
        );
        let _ = std::fs::remove_dir_all(&dir);
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
            .call_with_context(&context, request(shell("echo before; sleep 30")))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "cancelled");
        assert_eq!(structured["cancelled"], true);
        assert_eq!(
            structured["stdout"], "before\n",
            "the output produced before the cancellation is kept (FR-TOOL-BASH-5)"
        );
    }

    #[tokio::test]
    async fn a_cancelled_context_never_spawns_the_process() {
        let dir = temp_dir("cancelled-first");
        let marker = dir.join("marker");
        let tool = CommandTool::new();
        let context = ToolContext::new();
        context.cancellation().cancel();
        let script = format!("touch {}", marker.display());
        let output = tool
            .call_with_context(&context, request(shell(&script)))
            .await
            .expect("execute");
        assert_eq!(structured(&output)["code"], "cancelled");
        assert!(!marker.exists(), "nothing was spawned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn output_is_streamed_back_as_progress() {
        let tool = CommandTool::new();
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        let output = tool
            .call_with_context(&context, request(shell("echo streamed")))
            .await
            .expect("execute");
        assert!(!output.is_error);

        let progress = sink.0.lock().expect("lock");
        let stdout: Vec<u8> = progress
            .iter()
            .filter(|entry| entry.stream == ToolStream::Stdout)
            .flat_map(|entry| entry.bytes.clone())
            .collect();
        assert_eq!(String::from_utf8_lossy(&stdout), "streamed\n");
        assert!(
            progress
                .iter()
                .any(|entry| entry.stream == ToolStream::Stdout && entry.last),
            "the stream is closed with a last marker"
        );
    }

    #[tokio::test]
    async fn a_grandchild_holding_the_pipes_does_not_hang_the_call() {
        let limits = CommandLimits {
            drain: Duration::from_millis(100),
            ..CommandLimits::default()
        };
        let tool = CommandTool::new().with_limits(limits);
        let output = tool
            .call(request(shell("sleep 30 & echo done")))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert_eq!(
            structured["stdout"], "done\n",
            "the output the group produced before the pipes stalled is kept"
        );
        assert_eq!(structured["code"], "success");
        let reason = structured["reason"].as_str().unwrap_or_default();
        assert!(
            reason.contains("pipes"),
            "the result says the pipes outlived the drain: {reason}"
        );
    }

    #[tokio::test]
    async fn the_summary_can_report_matched_patterns() {
        let tool = CommandTool::new();
        let request =
            request(shell("printf 'alpha\\nbeta\\ngamma\\n'")).with_summary_pattern("beta");
        let output = tool.call(request).await.expect("execute");
        let summary = structured(&output)["summary"]
            .as_str()
            .expect("summary")
            .to_owned();
        assert!(summary.contains("beta"));
        assert!(!summary.contains("alpha"));
    }

    #[tokio::test]
    async fn the_summary_sees_both_streams() {
        let tool = CommandTool::new();
        let request =
            request(shell("echo out; echo 'error: bad' 1>&2")).with_summary_pattern("error");
        let output = tool.call(request).await.expect("execute");
        let summary = structured(&output)["summary"]
            .as_str()
            .expect("summary")
            .to_owned();
        assert!(
            summary.contains("error: bad"),
            "a pattern is matched against stderr too: {summary}"
        );
    }

    #[tokio::test]
    async fn the_summary_tails_the_real_end_of_a_truncated_stream() {
        let tool = CommandTool::new();
        let request = request(shell("for i in $(seq 1 200); do echo line-$i; done"))
            .with_output_cap(64)
            .with_summary_lines(2);
        let output = tool.call(request).await.expect("execute");
        let structured = structured(&output);
        assert_eq!(structured["truncated"], true);
        let summary = structured["summary"].as_str().expect("summary");
        assert!(
            summary.contains("line-200") && summary.contains("line-199"),
            "the tail is the end of what the process wrote, not the end of the capped head: {summary}"
        );
    }

    #[tokio::test]
    async fn secrets_are_redacted_out_of_the_model_text() {
        let tool = CommandTool::new().with_secrets(["hunter2hunter2"]);
        let output = tool
            .call(request(shell("printf 'token=%s\\n' hunter2hunter2")))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert_eq!(structured["stdout"], "token=[redacted]\n");
        assert!(structured["reason"].is_null());
    }

    #[tokio::test]
    async fn a_secret_is_redacted_before_the_cap_can_split_it() {
        let tool = CommandTool::new().with_secrets(["SECRETVALUE"]);
        let request = request(shell("printf 'xxxxxxxxxxSECRETVALUEyyyy'")).with_output_cap(12);
        let output = tool.call(request).await.expect("execute");
        let stdout = structured(&output)["stdout"]
            .as_str()
            .expect("stdout")
            .to_owned();
        assert!(
            !stdout.contains("SECRET"),
            "the cap cuts the redacted stream, never the raw one: {stdout}"
        );
        assert!(stdout.starts_with("xxxxxxxxxx[r"), "{stdout}");
    }

    #[tokio::test]
    async fn a_secret_never_reaches_the_progress_stream() {
        let tool = CommandTool::new().with_secrets(["hunter2hunter2"]);
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        tool.call_with_context(&context, request(shell("echo hunter2hunter2")))
            .await
            .expect("execute");
        let progress = sink.0.lock().expect("lock");
        let bytes: Vec<u8> = progress
            .iter()
            .flat_map(|entry| entry.bytes.clone())
            .collect();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(!text.contains("hunter2hunter2"), "{text}");
        assert!(text.contains("[redacted]"), "{text}");
    }

    #[tokio::test]
    async fn the_request_digest_does_not_carry_a_secret() {
        let tool = CommandTool::new().with_secrets(["s3cr3t"]);
        let with_secret = tool
            .call(request(shell("echo s3cr3t")))
            .await
            .expect("execute");
        let redacted = tool
            .call(request(shell("echo [redacted]")))
            .await
            .expect("execute");
        assert_eq!(
            structured(&with_secret)["request_digest"],
            structured(&redacted)["request_digest"],
            "the digest identifies the request as the executor saw it, secret redacted"
        );
    }

    #[test]
    fn a_secret_split_across_chunks_is_never_emitted() {
        let mut redactor = Redactor::new(Arc::from(vec!["hunter2".to_owned()]));
        let mut emitted = Vec::new();
        for byte in b"say hunter2 now".chunks(1) {
            emitted.extend_from_slice(&redactor.feed(byte));
        }
        emitted.extend_from_slice(&redactor.finish());
        assert_eq!(String::from_utf8_lossy(&emitted), "say [redacted] now");
    }

    #[tokio::test]
    async fn an_overlapping_shorter_secret_does_not_leave_the_tail_of_a_longer_one() {
        let tool = CommandTool::new().with_secrets(["abc", "abcdef"]);
        let output = tool
            .call(request(shell("echo abcdef")))
            .await
            .expect("execute");
        assert_eq!(
            structured(&output)["stdout"],
            "[redacted]\n",
            "the tool redacts the longest secret first, whatever order it was given them in"
        );
    }

    #[tokio::test]
    async fn the_audit_record_carries_the_digests_and_timestamps() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("echo audited")))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert!(structured["request_digest"].is_string());
        assert!(structured["output_hash"].is_string());
        assert!(structured["started_at"].is_string());
        assert!(structured["ended_at"].is_string());
        assert_eq!(
            structured["output_hash"],
            Digest::of(b"audited\n").to_string(),
            "the output hash is taken over the text the result carries (FR-TOOL-BASH-8)"
        );
    }

    #[tokio::test]
    async fn the_output_hash_covers_both_streams() {
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("echo out; echo err 1>&2")))
            .await
            .expect("execute");
        assert_eq!(
            structured(&output)["output_hash"],
            Digest::of(b"out\nerr\n").to_string()
        );
    }

    #[tokio::test]
    async fn a_request_cannot_override_a_host_variable() {
        let tool = CommandTool::new().with_env(BTreeMap::from([(
            "FROM_HOST".to_owned(),
            "host".to_owned(),
        )]));
        let request = request(shell("echo $FROM_HOST")).with_env("FROM_HOST", "request");
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "host\n");
    }

    #[tokio::test]
    async fn the_process_does_not_inherit_the_ambient_environment() {
        // The test process has an environment; the child must see none of it
        // unless the host or the request put it there (FR-TOOL-BASH-6).
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("printf '[%s]' \"${HOME:-}\"")))
            .await
            .expect("execute");
        assert_eq!(
            structured(&output)["stdout"],
            "[]",
            "the child saw the ambient HOME={}",
            home.to_string_lossy()
        );
    }

    #[tokio::test]
    async fn a_request_may_add_a_variable_the_host_did_not_set() {
        let tool = CommandTool::new();
        let request = request(shell("echo $FROM_REQUEST")).with_env("FROM_REQUEST", "added");
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "added\n");
    }

    #[tokio::test]
    async fn stdin_can_be_inline_text() {
        let tool = CommandTool::new();
        let request = request(shell("read -r line; printf %s \"$line\"")).with_stdin("hello\n");
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "hello");
    }

    #[tokio::test]
    async fn stdin_can_come_from_a_blob() {
        let blobs = Arc::new(Blobs::default());
        let id = blobs.put(b"hello from a blob").await.expect("put");
        let tool = CommandTool::new().with_blobs(blobs);
        let request = request(shell("read -r line; printf %s \"$line\"")).with_stdin_blob(id);
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "hello from a blob");
    }

    #[tokio::test]
    async fn a_stdin_blob_without_a_store_is_invalid_input() {
        let tool = CommandTool::new();
        let request = request(shell("cat")).with_stdin_blob(BlobId::of(b"x"));
        let output = tool.call(request).await.expect("execute");
        let structured = structured(&output);
        assert_eq!(structured["code"], "invalid_input");
        assert!(
            structured["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("blob store")
        );
    }

    #[tokio::test]
    async fn a_blob_that_is_not_there_is_invalid_input() {
        let tool = CommandTool::new().with_blobs(Arc::new(Blobs::default()));
        let request = request(shell("cat")).with_stdin_blob(BlobId::of(b"absent"));
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["code"], "invalid_input");
    }

    #[tokio::test]
    async fn stderr_can_be_merged_into_stdout() {
        let tool = CommandTool::new();
        let request = request(shell("echo out; echo err 1>&2")).merging_stderr();
        let output = tool.call(request).await.expect("execute");
        let structured = structured(&output);
        let stdout = structured["stdout"].as_str().expect("stdout");
        assert!(stdout.contains("out"));
        assert!(stdout.contains("err"));
        assert_eq!(structured["stderr"], "");
    }

    #[tokio::test]
    async fn the_cwd_is_honoured() {
        let dir = temp_dir("cwd");
        let tool = CommandTool::new();
        let output = tool
            .call(request(shell("pwd")).with_cwd(&dir))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert!(
            structured["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains(&file_name(&dir)),
            "the process ran in the directory the request named"
        );
        assert_eq!(structured["cwd"], dir.to_string_lossy().as_ref());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_fields_are_rejected_before_the_tool_runs() {
        let (registry, name) = registered();
        let error = registry
            .execute(
                &ToolContext::new(),
                &name,
                serde_json::json!({
                    "command": { "kind": "program", "program": "true" },
                    "oops": 1
                }),
            )
            .await
            .expect_err("the request does not parse");
        assert!(error.to_string().contains("oops"), "{error}");
    }

    #[tokio::test]
    async fn unknown_fields_inside_the_command_line_are_rejected() {
        let (registry, name) = registered();
        let error = registry
            .execute(
                &ToolContext::new(),
                &name,
                serde_json::json!({
                    "command": { "kind": "program", "program": "true", "oops": 1 }
                }),
            )
            .await
            .expect_err("the command line does not parse");
        assert!(error.to_string().contains("oops"), "{error}");
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let (registry, name) = registered();
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

    fn registered() -> (rustychain_core::ToolRegistry, rustychain_core::ToolName) {
        let mut registry = rustychain_core::ToolRegistry::new();
        registry
            .register_tool(&CommandTool::new())
            .expect("register command");
        let name = rustychain_core::ToolName::new("command").expect("name");
        (registry, name)
    }

    fn request(command: CommandLine) -> CommandRequest {
        let mut request = CommandRequest::program("true");
        request.command = command;
        request
    }

    fn program(program: &str, args: &[&str]) -> CommandLine {
        CommandLine::Program {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    fn shell(script: &str) -> CommandLine {
        CommandLine::Shell {
            shell: DEFAULT_SHELL.to_owned(),
            script: script.to_owned(),
        }
    }

    fn structured(output: &ToolOutput) -> Value {
        output.structured.clone().expect("structured")
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rustychain-command-{}-{label}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn file_name(path: &std::path::Path) -> String {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Waits for the process to record its own id, so the test never races the
    /// child's first write.
    fn read_pid(path: &std::path::Path) -> i32 {
        for _ in 0..500 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if let Ok(pid) = text.trim().parse() {
                    return pid;
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the process never recorded its pid at {}", path.display());
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<ToolProgress>>);

    impl rustychain_core::ProgressSink for Sink {
        fn send(&self, progress: ToolProgress) {
            self.0.lock().expect("lock").push(progress);
        }
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
}
