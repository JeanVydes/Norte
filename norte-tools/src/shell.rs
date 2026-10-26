//! The flagship tool of the suite: a bounded command executor.
//!
//! The executor runs one program or one shell script, streams what it writes,
//! keeps that output bounded, and reports a structured outcome (FR-TOOL-BASH-1
//! to FR-TOOL-BASH-8). Four properties shape the code:
//!
//! * **The host bounds; the request may only ask for less.** Timeouts, output
//!   caps and the size of the text that reaches the model come from
//!   [`ShellLimits`]; a request that asks for more is clamped, never obeyed.
//! * **The host decides; the request cannot widen.** The process starts from the
//!   host's environment and cannot override a variable the host set, and the
//!   secrets the host declares are redacted from output, from the live stream
//!   and from the audit record whether the request likes it or not.
//! * **The process group does not outlive the call.** On timeout, on
//!   cancellation, when a pipe will not close and when the future is dropped,
//!   the whole group is killed. A call a host allowed to run unbounded is the
//!   one exception, and it is not really one: nothing bounds *it*, but the same
//!   guard, the same cancellation token and the same drop still kill its group.
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
use norte_core::{
    BlobId, BlobStore, Digest, Error, FnExecutor, Part, Result, ToolContext, ToolDeclaration,
    ToolMetadata, ToolOutput, ToolProgress, ToolStream,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::time::timeout as with_timeout;

/// The shell [`ShellRequest::shell`] runs with unless it names another one.
pub const DEFAULT_SHELL: &str = "sh";

/// The shell a [`ShellRequest::shell`] field gets when it is omitted.
fn default_shell() -> String {
    DEFAULT_SHELL.to_owned()
}

/// What a redacted secret becomes in output, progress and the audit record.
const REDACTED: &[u8] = b"[redacted]";

/// How much one read from a pipe asks for.
const READ_BUFFER: usize = 8192;

/// What to run and how. Everything here is a *request*: each bound is clamped
/// against the host's [`ShellLimits`], and no field can widen what the host
/// allows (FR-TOOL-BASH-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
#[schemars(
    description = "The command to run and the bounds for it. The host caps the run time and the output size, so asking for more than it allows is clamped, not honoured."
)]
pub struct ShellRequest {
    /// The script to run, passed to the shell with `-c`.
    #[schemars(description = "The script to run.", example = &"cargo test")]
    pub script: String,
    /// The interpreter to run it with. `sh` when the request omits one.
    #[serde(default = "default_shell")]
    #[schemars(
        default = "default_shell",
        description = "The interpreter; defaults to sh."
    )]
    pub shell: String,
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
    ///
    /// Zero asks for no bound at all, which only a host that set
    /// [`ShellLimits::allow_indefinite`] grants; anywhere else it is the host's
    /// default, because an unclamped zero would be a request to run forever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "How long the process may run, in milliseconds. Above the host's ceiling it is clamped to it. Zero asks for no bound, which the host grants only where it allows it, for a call that starts something long-lived; a foreground call that asks for zero is given the host's default instead."
    )]
    pub timeout_ms: Option<u64>,
    /// How many bytes of each stream the executor keeps. A value above the
    /// host's ceiling is clamped to it; the model sees at most
    /// [`ShellLimits::max_model_bytes`] of it.
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

impl ShellRequest {
    /// A request that runs `script` through `shell`.
    #[must_use]
    pub fn shell(script: impl Into<String>) -> Self {
        Self {
            script: script.into(),
            shell: DEFAULT_SHELL.to_owned(),
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

    /// Runs the script through another shell.
    #[must_use]
    pub fn with_shell(mut self, shell: impl Into<String>) -> Self {
        self.shell = shell.into();
        self
    }

    /// A directory to run the process in.
    #[must_use]
    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Adds one environment variable.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Sets the time bound.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX));
        self
    }

    /// Sets how many bytes of each stream the executor keeps.
    #[must_use]
    pub fn with_output_cap(mut self, bytes: usize) -> Self {
        self.max_output_bytes = Some(bytes);
        self
    }

    /// Sets the process's standard input.
    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<String>) -> Self {
        self.stdin = Some(stdin.into());
        self
    }

    /// Sets a blob as the standard input, which wins over [`with_stdin`] when
    /// both are set.
    ///
    /// [`with_stdin`]: Self::with_stdin
    #[must_use]
    pub fn with_stdin_blob(mut self, blob: BlobId) -> Self {
        self.stdin_blob = Some(blob);
        self
    }

    /// Merges stderr into stdout.
    #[must_use]
    pub fn merging_stderr(mut self) -> Self {
        self.merge_stderr = true;
        self
    }

    /// Includes the last `lines` lines of the output in the summary.
    #[must_use]
    pub fn with_summary_lines(mut self, lines: usize) -> Self {
        self.summary_lines = Some(lines);
        self
    }

    /// Reports the output lines containing `pattern` in the summary.
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
pub enum ShellCode {
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
    /// The caller stopped waiting and the process was left running.
    Detached,
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
// The four flags restate `code`, which already says how the call ended. They
// are here because the wire carries them and the renderers read them; a reader
// that wants the reason matches on `code` instead.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellOutput {
    /// How the invocation ended.
    pub code: ShellCode,
    /// The process's exit code, when it exited on its own.
    pub exit_code: Option<i32>,
    /// The signal that ended the process, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Standard output, redacted and bounded. When the executor had to cut
    /// something, this text carries a marker saying so.
    pub stdout: String,
    /// Standard error, redacted and bounded; empty when the request merged it.
    pub stderr: String,
    /// How long the process ran, in milliseconds.
    pub duration_ms: u64,
    /// Bytes the process wrote in total, across both streams, whether or not
    /// they were kept.
    pub bytes_read: usize,
    /// Whether any of what the process wrote was left out of this result.
    pub truncated: bool,
    /// Whether the host's time bound killed the process.
    ///
    /// It, like `cancelled` and `detached`, restates `code` for readers that
    /// want a flag rather than a match. They are kept because the wire and the
    /// renderers already read them, not because the state is not also in
    /// `code`.
    pub timed_out: bool,
    /// Whether the caller cancelled the invocation.
    pub cancelled: bool,
    /// Whether the caller stopped waiting and the process was left running.
    ///
    /// The process outlives this call: its output belongs to whoever detached
    /// it, and `stdout`/`stderr` hold whatever arrived before the detach.
    #[serde(default)]
    pub detached: bool,
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
    /// When the process was started.
    pub started_at: DateTime<Utc>,
    /// When the invocation stopped.
    pub ended_at: DateTime<Utc>,
}

impl ShellOutput {
    fn failed(
        code: ShellCode,
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
            detached: false,
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

/// Packs a [`ShellOutput`] into the text and structure the model receives.
pub trait ShellRenderer: Send + Sync {
    /// Packs `output` into the text the model receives.
    fn render(&self, output: &ShellOutput) -> String;
}

/// The default context renderer: enough for a model to decide what to do next.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlainShellRenderer;

impl ShellRenderer for PlainShellRenderer {
    fn render(&self, output: &ShellOutput) -> String {
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
pub struct ShellLimits {
    /// Used when the request does not ask for a time bound.
    pub default_timeout: Duration,
    /// Ceiling for a request's `timeout_ms`.
    pub max_timeout: Duration,
    /// Whether a request of `0` is granted, meaning no bound at all.
    ///
    /// Off by default, because a call with no bound cannot be told from a hung
    /// one by whoever is waiting for it. A host turns it on for the calls it
    /// runs in the background, where nothing waits: a server started for the rest
    /// of the session is the case it exists for. The process is still the host's
    /// to kill — the group guard, the cancellation token and the detach token all
    /// reach it exactly as they do a bounded call.
    pub allow_indefinite: bool,
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

impl Default for ShellLimits {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(120),
            max_timeout: Duration::from_secs(600),
            allow_indefinite: false,
            grace: Duration::from_secs(2),
            drain: Duration::from_secs(2),
            max_output_bytes: 1024 * 1024,
            max_model_bytes: 32 * 1024,
            tail_bytes: 64 * 1024,
        }
    }
}

/// The flagship tool of the default suite: a production-grade command executor.
///
/// It runs a shell script and nothing else. A program with arguments is a shell
/// script too — `cargo test` is one — so the schema is flat and there is no
/// variant for the model to get wrong: no discriminator to forget, no `args`
/// array that only applies half the time.
#[norte_core::tool(
    name = "shell",
    description = "Run a shell script. Returns exit status, bounded stdout/stderr and a structured summary. It is a real shell, so pipes, redirection, variables and && all work."
)]
#[derive(Clone)]
pub struct ShellTool {
    limits: ShellLimits,
    renderer: Arc<dyn ShellRenderer>,
    blobs: Option<Arc<dyn BlobStore>>,
    env: BTreeMap<String, String>,
    secrets: Arc<[String]>,
}

impl ShellTool {
    /// A command tool with the default limits, the plain renderer and no blob
    /// store.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: ShellLimits::default(),
            renderer: Arc::new(PlainShellRenderer),
            blobs: None,
            env: BTreeMap::new(),
            secrets: Arc::from(Vec::new()),
        }
    }

    /// Sets the host's bounds.
    #[must_use]
    pub fn with_limits(mut self, limits: ShellLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets the renderer that shapes the text the model receives.
    #[must_use]
    pub fn with_renderer(mut self, renderer: Arc<dyn ShellRenderer>) -> Self {
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
    pub fn into_declaration(self) -> ToolDeclaration<ShellRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    /// The time bound for this request, or `None` when it asked for none and the
    /// host allows that.
    ///
    /// Three cases, in order: no bound requested and allowed; a bound asked for,
    /// clamped to the host's ceiling; and everything else, which gets the host's
    /// default. A request of `0` asks for no bound — a zero-millisecond run would
    /// kill every process before it started, so the number would be useless as
    /// anything but a sentinel — and it is granted only where
    /// [`ShellLimits::allow_indefinite`] says so.
    ///
    /// `None` is not "no limit by accident": a call with no bound runs until it
    /// ends, is cancelled, or is detached, and the caller has no way to make the
    /// difference between it and a hung one. That is why the host decides.
    fn timeout(&self, request: &ShellRequest) -> Option<Duration> {
        let Some(millis) = request.timeout_ms else {
            return Some(self.limits.default_timeout);
        };
        // Zero is the sentinel, and it is granted only where the host said so.
        // Everywhere else it falls through to the default beside it, because a
        // request that asked for no bound and was refused still has to be given
        // one.
        if millis == 0 {
            return if self.limits.allow_indefinite {
                None
            } else {
                Some(self.limits.default_timeout)
            };
        }
        Some(Duration::from_millis(millis).min(self.limits.max_timeout))
    }

    /// The capture cap for this request, clamped to the host's ceiling.
    fn output_cap(&self, request: &ShellRequest) -> usize {
        request
            .max_output_bytes
            .unwrap_or(self.limits.max_output_bytes)
            .min(self.limits.max_output_bytes)
    }

    /// The request with every declared secret replaced, for the audit digest:
    /// a digest of the raw request would leak a secret to anyone who can guess
    /// the rest of it.
    fn digest(&self, request: &ShellRequest) -> Result<Digest> {
        let mut value = serde_json::to_value(request)?;
        redact_value(&mut value, &self.secrets);
        Ok(Digest::of_json(&value))
    }

    async fn stdin(&self, request: &ShellRequest) -> Result<Vec<u8>> {
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

    fn build_command(&self, request: &ShellRequest) -> Command {
        let mut command = Command::new(&request.shell);
        command.arg("-c").arg(&request.script);
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

    /// The answer to a call that was detached while it ran.
    ///
    /// It reports what has arrived, not what the process will produce: the
    /// process is still alive and its output belongs to whoever detached it.
    /// The exit status is absent because there is none yet — the call did not
    /// end, it was let go of.
    #[allow(clippy::too_many_arguments)]
    fn detached_output(
        &self,
        request: &ShellRequest,
        snapshot: &(Arc<Mutex<Captured>>, Arc<Mutex<Captured>>),
        cap: usize,
        clock: &Clock,
        command_line: &str,
    ) -> Result<ToolOutput> {
        let stdout = snapshot.0.lock().expect("command stdout").clone();
        let stderr = snapshot.1.lock().expect("command stderr").clone();
        // A detached call does not report the merged view: its reader is still
        // writing into it, and two streams reported apart say more than one
        // that is still filling up.
        let merged = None;
        let decoded = decode(&Decode {
            request,
            stdout: &stdout,
            stderr: &stderr,
            merged: merged.as_ref(),
            cap,
            budget: self.limits.max_model_bytes,
        });
        let mut output_hash = decoded.stdout.as_bytes().to_vec();
        output_hash.extend_from_slice(decoded.stderr.as_bytes());
        let output = ShellOutput {
            code: ShellCode::Detached,
            exit_code: None,
            signal: None,
            stdout: decoded.stdout,
            stderr: decoded.stderr,
            duration_ms: clock.elapsed_ms(),
            bytes_read: decoded.bytes_read,
            truncated: decoded.truncated,
            timed_out: false,
            cancelled: false,
            detached: true,
            cwd: request.cwd.clone(),
            summary: decoded.summary,
            reason: Some(
                "the caller stopped waiting; the process is still running and what it \
                 writes from now on is not part of this result"
                    .to_owned(),
            ),
            request_digest: clock.digest,
            output_hash: Digest::of(&output_hash),
            started_at: clock.started_at,
            ended_at: Utc::now(),
        };
        self.report(&output, command_line)
    }

    fn report(&self, output: &ShellOutput, command_line: &str) -> Result<ToolOutput> {
        audit(output, command_line);
        render(&*self.renderer, output)
    }

    async fn run(&self, context: &ToolContext, request: &ShellRequest) -> Result<ToolOutput> {
        let clock = Clock::start(self.digest(request)?);
        let command_line = redact(
            &describe_command(&request.shell, &request.script),
            &self.secrets,
        );

        if context.is_cancelled() {
            let output = clock.failed(ShellCode::Cancelled, "cancelled before the process started");
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
                let output = clock.failed(ShellCode::SpawnFailed, reason);
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
        request: &ShellRequest,
        mut child: tokio::process::Child,
        clock: &Clock,
        command_line: &str,
    ) -> Result<ToolOutput> {
        let pid = child.id();
        let mut guard = GroupGuard::new(pid);
        let (streams, cap) = self.start_readers(context, request, &mut child);
        // Kept beside `streams` for the detached path, which reads what has
        // arrived without waiting for the readers to finish.
        let snapshot = (Arc::clone(&streams.stdout), Arc::clone(&streams.stderr));

        let wait = self
            .wait_for_exit(context, request, &mut child, &mut guard)
            .await;
        let (status, timed_out, cancelled, detached) =
            (wait.status, wait.timed_out, wait.cancelled, wait.detached);

        // A detached call reports what has arrived so far and returns without
        // draining: the readers belong to a process that is still running, and
        // waiting for them would defeat the point of detaching.
        //
        // The `Child` and the readers have to outlive this function for that to
        // be true. `kill_on_drop` is set on the command, so letting the `Child`
        // fall out of scope would kill the very process the caller just asked to
        // leave alone. A task holds them instead, and ends when the process does.
        if detached {
            let detector = Detached {
                child,
                stdout_task: streams.stdout_task,
                stderr_task: streams.stderr_task,
            };
            tokio::spawn(detector.hold());
            return self.detached_output(request, &snapshot, cap, clock, command_line);
        }

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

        let output = ShellOutput {
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
            detached: false,
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
        request: &ShellRequest,
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

    /// Waits for the process to end, or for one of the three things that can end
    /// the wait for it.
    ///
    /// The three are not the same. Cancelling and timing out kill the group;
    /// detaching does not — it stops waiting and leaves the process running, so
    /// whoever detached it reads the rest later.
    ///
    /// A call the host allowed no bound has no deadline to reach: the timeout
    /// branch never completes, and the call ends the way every other one can —
    /// the process exits, the caller cancels, or the caller detaches.
    async fn wait_for_exit(
        &self,
        context: &ToolContext,
        request: &ShellRequest,
        child: &mut tokio::process::Child,
        guard: &mut GroupGuard,
    ) -> Wait {
        let deadline = self.timeout(request);
        let bounded = async move {
            match deadline {
                Some(bound) => tokio::time::sleep(bound).await,
                None => std::future::pending::<()>().await,
            }
        };

        let mut wait = Wait::default();
        wait.status = tokio::select! {
            status = child.wait() => status,
            () = bounded => {
                wait.timed_out = true;
                self.terminate(child, guard).await
            }
            () = context.cancelled() => {
                wait.cancelled = true;
                self.terminate(child, guard).await
            }
            () = context.detached() => {
                wait.detached = true;
                // Disarming the guard is what lets the process outlive this
                // call: the guard kills its group on drop.
                guard.disarm();
                Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "detached",
                ))
            }
        };
        wait
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

/// How a call stopped waiting, and what the process did.
///
/// The three flags are separate because they are not the same answer: the first
/// two killed the group and the third handed it on. `status` is the process's
/// own when it ended of its own accord, and the error from a kill otherwise.
struct Wait {
    status: std::io::Result<ExitStatus>,
    timed_out: bool,
    cancelled: bool,
    detached: bool,
}

impl Default for Wait {
    fn default() -> Self {
        Self {
            // The process would have to answer for itself before this is read;
            // one that never started is handled before the wait.
            status: Err(std::io::Error::other("the process was not waited for")),
            timed_out: false,
            cancelled: false,
            detached: false,
        }
    }
}

/// What a detached call hands to a task, so the process is not killed by the
/// `Child` being dropped.
///
/// `kill_on_drop` is what makes an ordinary call safe: a dropped future leaves
/// no process behind. Detaching asks for the opposite, and the only way to have
/// both is to give the `Child` an owner that outlives the call.
struct Detached {
    child: tokio::process::Child,
    stdout_task: tokio::task::JoinHandle<()>,
    stderr_task: tokio::task::JoinHandle<()>,
}

impl Detached {
    /// Waits for the process and its readers, then lets go.
    ///
    /// Nothing reports the result: the call that would have has already
    /// answered. The task exists to keep the process alive until it ends of its
    /// own accord, which is the whole of what detaching promised.
    async fn hold(mut self) {
        let _ = self.child.wait().await;
        let _ = self.stdout_task.await;
        let _ = self.stderr_task.await;
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

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<ShellRequest, ToolOutput> for ShellTool {
    async fn call(&self, request: ShellRequest) -> Result<ToolOutput> {
        self.run(&ToolContext::new(), &request).await
    }

    async fn call_with_context(
        &self,
        context: &ToolContext,
        request: ShellRequest,
    ) -> Result<ToolOutput> {
        self.run(context, &request).await
    }
}

fn render(renderer: &dyn ShellRenderer, output: &ShellOutput) -> Result<ToolOutput> {
    let body = renderer.render(output);
    let structured = serde_json::to_value(output)?;
    let is_error = !matches!(output.code, ShellCode::Success);
    Ok(ToolOutput {
        parts: vec![Part::text(body)],
        structured: Some(structured),
        is_error,
    })
}

/// The invocation as one line, for the audit record. It is redacted before it
/// is logged, because an argument is often where a secret lives.
fn describe_command(shell: &str, script: &str) -> String {
    format!("{shell} -c {script}")
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

    fn failed(&self, code: ShellCode, reason: impl Into<String>) -> ShellOutput {
        ShellOutput::failed(
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
    request: &'a ShellRequest,
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
    request: &ShellRequest,
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
) -> (ShellCode, Option<i32>, Option<i32>) {
    let (exit_code, signal) = match status {
        Ok(status) => decompose_status(*status),
        Err(_) => (None, None),
    };
    let code = if cancelled {
        ShellCode::Cancelled
    } else if timed_out {
        ShellCode::Timeout
    } else if read_failed {
        ShellCode::IoError
    } else {
        match status {
            Ok(status) if status.success() => ShellCode::Success,
            Ok(_) if signal.is_some() => ShellCode::Signaled,
            Ok(_) => ShellCode::NonZeroExit,
            Err(_) => ShellCode::IoError,
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

fn stdin_code(error: &Error) -> ShellCode {
    match error {
        Error::BlobNotFound(_) | Error::InvalidInput(_) => ShellCode::InvalidInput,
        _ => ShellCode::IoError,
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
/// [`ShellTool::with_secrets`] leaves them: replacing a shorter secret first
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

fn audit(output: &ShellOutput, command_line: &str) {
    tracing::info!(
        target: "norte.audit",
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
pub fn shell_tool() -> Arc<dyn norte_core::AnyFunction> {
    Arc::new(ShellTool::new().into_declaration())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[tokio::test]
    async fn captures_stdout_and_reports_success() {
        let tool = ShellTool::new();
        let output = tool.call(request("echo hello")).await.expect("execute");
        assert!(!output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "success");
        assert_eq!(structured["stdout"], "hello\n");
        assert_eq!(structured["bytes_read"], 6);
        assert!(structured["reason"].is_null());
    }

    #[tokio::test]
    async fn a_non_zero_exit_is_reported_not_raised() {
        let tool = ShellTool::new();
        let output = tool.call(request("exit 7")).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "non_zero_exit");
        assert_eq!(structured["exit_code"], 7);
    }

    #[tokio::test]
    async fn a_signal_death_is_its_own_code() {
        let tool = ShellTool::new();
        let output = tool.call(request("kill -KILL $$")).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "signaled");
        #[cfg(unix)]
        assert_eq!(structured["signal"], 9);
    }

    #[tokio::test]
    async fn a_missing_program_is_reported_by_the_shell() {
        // With a shell in front, a missing binary is the shell's own error:
        // it starts, reports `command not found`, exits 127. That is a
        // non-zero exit and not a spawn failure, and the model still learns
        // which name was wrong from stderr.
        let tool = ShellTool::new();
        let output = tool
            .call(request("definitely-not-a-real-binary-xyz"))
            .await
            .expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "non_zero_exit");
        assert_eq!(structured["exit_code"], 127);
        let stderr = structured["stderr"].as_str().expect("stderr");
        assert!(
            stderr.contains("definitely-not-a-real-binary-xyz"),
            "the shell names what it could not find: {stderr}"
        );
    }

    #[tokio::test]
    async fn an_interpreter_that_does_not_exist_is_a_spawn_failure() {
        // This is the remaining spawn failure: the shell itself cannot start.
        let tool = ShellTool::new();
        let mut request = request("true");
        request.shell = "definitely-not-a-real-shell-xyz".to_owned();
        let output = tool.call(request).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "spawn_failed");
        let reason = structured["reason"].as_str().expect("reason");
        assert!(
            reason.contains("definitely-not-a-real-shell-xyz"),
            "the reason names what could not start: {reason}"
        );
    }

    #[tokio::test]
    async fn excess_output_is_truncated_and_marked() {
        let tool = ShellTool::new();
        let request =
            request("for i in $(seq 1 200); do echo '0123456789'; done").with_output_cap(64);
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
        let limits = ShellLimits {
            max_model_bytes: 256,
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let output = tool
            .call(request("printf 'HEAD\\n'; seq 1 500; printf 'TAIL\\n'"))
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
        let tool = ShellTool::new();
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        let request = request("seq 1 500").with_output_cap(64);
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
        let tool = ShellTool::new();
        let request = request("sleep 30").with_timeout(Duration::from_millis(50));
        let output = tool.call(request).await.expect("execute");
        assert!(output.is_error);
        let structured = structured(&output);
        assert_eq!(structured["code"], "timeout");
        assert_eq!(structured["timed_out"], true);
    }

    #[tokio::test]
    async fn zero_asks_for_no_bound_and_gets_the_host_default_by_default() {
        // `0` is a sentinel, not a duration: a zero-millisecond bound would kill
        // every process before it started. A host that has not opted in reads it
        // as "no opinion" and applies its default, which is what keeps the
        // number from being a hole a model can disappear into.
        let limits = ShellLimits {
            default_timeout: Duration::from_millis(120),
            grace: Duration::from_millis(50),
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let request = request("sleep 30").with_timeout(Duration::ZERO);
        let output = tool.call(request).await.expect("execute");
        assert_eq!(
            structured(&output)["code"],
            "timeout",
            "an opt-out host applied its default of 120 ms"
        );
    }

    #[tokio::test]
    async fn a_host_that_allows_it_lets_zero_run_to_the_end() {
        // The case this exists for: something that starts a long-lived server and
        // answers. It has to outlive the default by a wide margin, or the test
        // would pass on a build that ignored the flag.
        let limits = ShellLimits {
            default_timeout: Duration::from_millis(80),
            grace: Duration::from_millis(50),
            allow_indefinite: true,
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let request = request("sleep 0.4; echo done").with_timeout(Duration::ZERO);
        let output = tool.call(request).await.expect("execute");
        let structured = structured(&output);
        assert_eq!(
            structured["code"], "success",
            "it outlived the 80 ms default: {structured:?}"
        );
        assert_eq!(
            structured["stdout"], "done\n",
            "its output arrived at the end of the run"
        );
        assert!(
            structured["duration_ms"].as_u64().unwrap_or(0) >= 400,
            "it really ran past the default: {structured:?}"
        );
    }

    #[tokio::test]
    async fn an_unbounded_call_is_still_cancelled_and_its_group_killed() {
        // "No bound" means no bound, not no control: whoever allowed it can still
        // cancel, and the group guard still runs. This is the property the module
        // promises, and the flag must not be a way around it.
        let dir = temp_dir("unbounded-cancel");
        let pid_file = dir.join("pid");
        let limits = ShellLimits {
            allow_indefinite: true,
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let script = format!("echo $$ > {}; sleep 30", pid_file.display());
        let request = request(&script).with_timeout(Duration::ZERO);

        let context = ToolContext::new();
        let cancel = context.cancellation();
        let watched = pid_file.clone();
        tokio::spawn(async move {
            // Cancel only once the process has written its pid, so the assertion
            // below is not a race against startup.
            wait_for(&watched).await;
            cancel.cancel();
        });

        let output = tool
            .call_with_context(&context, request)
            .await
            .expect("execute");
        assert_eq!(
            structured(&output)["code"],
            "cancelled",
            "the caller ended it, not a deadline"
        );

        // The group is gone: killed on the way out, unbounded or not.
        let pid = read_pid(&pid_file);
        #[cfg(unix)]
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
            "the group of an unbounded call is killed like any other"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_request_cannot_extend_the_host_ceiling_on_time() {
        let limits = ShellLimits {
            max_timeout: Duration::from_millis(200),
            grace: Duration::from_millis(50),
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let request = request("sleep 30").with_timeout(Duration::from_secs(600));
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
        let limits = ShellLimits {
            grace: Duration::from_millis(100),
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let script = format!("echo $$ > {}; trap '' TERM; sleep 30", pid_file.display());
        let request = request(&script).with_timeout(Duration::from_millis(100));
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
        let dir = temp_dir("cancelled-midway");
        let marker = dir.join("marked");
        let watched = marker.clone();
        let tool = ShellTool::new();
        let context = ToolContext::new();
        let signal = context.cancellation();
        tokio::spawn(async move {
            // Cancel only once `echo before` has run: the script touches the
            // marker *after* the output reached the pipe, so the assertion
            // below is not a race against process startup.
            wait_for(&watched).await;
            signal.cancel();
        });
        let script = format!("echo before; touch {}; sleep 30", marker.display());
        let output = tool
            .call_with_context(&context, request(&script))
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_detached_call_reports_and_leaves_the_process_running() {
        // The difference from cancelling, and the whole point: the call ends,
        // the process does not. It keeps writing after the call returned.
        let dir = temp_dir("detached");
        let marker = dir.join("marker");
        let later = dir.join("later");
        let tool = ShellTool::new();
        let context = ToolContext::new();
        let signal = context.detach();
        let watched = marker.clone();
        tokio::spawn(async move {
            // Detach only after the first line reached the pipe, so the
            // assertion on the captured output is not a race with startup.
            wait_for(&watched).await;
            signal.cancel();
        });

        let script = format!(
            "echo before; touch {}; sleep 1; touch {}",
            marker.display(),
            later.display()
        );
        let output = tool
            .call_with_context(&context, request(&script))
            .await
            .expect("execute");

        assert!(output.is_error, "a detached call is not a success");
        let structured = structured(&output);
        assert_eq!(structured["code"], "detached");
        assert_eq!(structured["detached"], true);
        assert_eq!(
            structured["stdout"], "before\n",
            "what arrived before the detach is kept"
        );
        // The process outlived the call: it writes its second marker a second
        // after the call returned.
        wait_for(&later).await;
        assert!(later.exists(), "the process kept running after the detach");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn a_cancelled_context_never_spawns_the_process() {
        let dir = temp_dir("cancelled-first");
        let marker = dir.join("marker");
        let tool = ShellTool::new();
        let context = ToolContext::new();
        context.cancellation().cancel();
        let script = format!("touch {}", marker.display());
        let output = tool
            .call_with_context(&context, request(&script))
            .await
            .expect("execute");
        assert_eq!(structured(&output)["code"], "cancelled");
        assert!(!marker.exists(), "nothing was spawned");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn output_is_streamed_back_as_progress() {
        let tool = ShellTool::new();
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        let output = tool
            .call_with_context(&context, request("echo streamed"))
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
        let limits = ShellLimits {
            drain: Duration::from_millis(100),
            ..ShellLimits::default()
        };
        let tool = ShellTool::new().with_limits(limits);
        let output = tool
            .call(request("sleep 30 & echo done"))
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
        let tool = ShellTool::new();
        let request = request("printf 'alpha\\nbeta\\ngamma\\n'").with_summary_pattern("beta");
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
        let tool = ShellTool::new();
        let request = request("echo out; echo 'error: bad' 1>&2").with_summary_pattern("error");
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
        let tool = ShellTool::new();
        let request = request("for i in $(seq 1 200); do echo line-$i; done")
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
        let tool = ShellTool::new().with_secrets(["hunter2hunter2"]);
        let output = tool
            .call(request("printf 'token=%s\\n' hunter2hunter2"))
            .await
            .expect("execute");
        let structured = structured(&output);
        assert_eq!(structured["stdout"], "token=[redacted]\n");
        assert!(structured["reason"].is_null());
    }

    #[tokio::test]
    async fn a_secret_is_redacted_before_the_cap_can_split_it() {
        let tool = ShellTool::new().with_secrets(["SECRETVALUE"]);
        let request = request("printf 'xxxxxxxxxxSECRETVALUEyyyy'").with_output_cap(12);
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
        let tool = ShellTool::new().with_secrets(["hunter2hunter2"]);
        let sink = Arc::new(Sink::default());
        let context = ToolContext::new().with_progress(sink.clone());
        tool.call_with_context(&context, request("echo hunter2hunter2"))
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
        let tool = ShellTool::new().with_secrets(["s3cr3t"]);
        let with_secret = tool.call(request("echo s3cr3t")).await.expect("execute");
        let redacted = tool
            .call(request("echo [redacted]"))
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
        let tool = ShellTool::new().with_secrets(["abc", "abcdef"]);
        let output = tool.call(request("echo abcdef")).await.expect("execute");
        assert_eq!(
            structured(&output)["stdout"],
            "[redacted]\n",
            "the tool redacts the longest secret first, whatever order it was given them in"
        );
    }

    #[tokio::test]
    async fn the_audit_record_carries_the_digests_and_timestamps() {
        let tool = ShellTool::new();
        let output = tool.call(request("echo audited")).await.expect("execute");
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
        let tool = ShellTool::new();
        let output = tool
            .call(request("echo out; echo err 1>&2"))
            .await
            .expect("execute");
        assert_eq!(
            structured(&output)["output_hash"],
            Digest::of(b"out\nerr\n").to_string()
        );
    }

    #[tokio::test]
    async fn a_request_cannot_override_a_host_variable() {
        let tool = ShellTool::new().with_env(BTreeMap::from([(
            "FROM_HOST".to_owned(),
            "host".to_owned(),
        )]));
        let request = request("echo $FROM_HOST").with_env("FROM_HOST", "request");
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
        let tool = ShellTool::new();
        let output = tool
            .call(request("printf '[%s]' \"${HOME:-}\""))
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
        let tool = ShellTool::new();
        let request = request("echo $FROM_REQUEST").with_env("FROM_REQUEST", "added");
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "added\n");
    }

    #[tokio::test]
    async fn stdin_can_be_inline_text() {
        let tool = ShellTool::new();
        let request = request("read -r line; printf %s \"$line\"").with_stdin("hello\n");
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "hello");
    }

    #[tokio::test]
    async fn stdin_can_come_from_a_blob() {
        let blobs = Arc::new(Blobs::default());
        let id = blobs.put(b"hello from a blob").await.expect("put");
        let tool = ShellTool::new().with_blobs(blobs);
        let request = request("read -r line; printf %s \"$line\"").with_stdin_blob(id);
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["stdout"], "hello from a blob");
    }

    #[tokio::test]
    async fn a_stdin_blob_without_a_store_is_invalid_input() {
        let tool = ShellTool::new();
        let request = request("cat").with_stdin_blob(BlobId::of(b"x"));
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
        let tool = ShellTool::new().with_blobs(Arc::new(Blobs::default()));
        let request = request("cat").with_stdin_blob(BlobId::of(b"absent"));
        let output = tool.call(request).await.expect("execute");
        assert_eq!(structured(&output)["code"], "invalid_input");
    }

    #[tokio::test]
    async fn stderr_can_be_merged_into_stdout() {
        let tool = ShellTool::new();
        let request = request("echo out; echo err 1>&2").merging_stderr();
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
        let tool = ShellTool::new();
        let output = tool
            .call(request("pwd").with_cwd(&dir))
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
                    "script": "true",
                    "oops": 1
                }),
            )
            .await
            .expect_err("the request does not parse");
        assert!(error.to_string().contains("oops"), "{error}");
    }

    #[tokio::test]
    async fn unknown_fields_inside_the_script_are_rejected() {
        // The schema is flat now, so the only place an unknown field can hide
        // is beside `script` — which the test above covers. This one keeps the
        // nested form rejected too: an old client sending `command` must fail
        // loudly rather than be silently understood.
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
            .expect_err("the nested form is not the schema");
        assert!(
            error.to_string().contains("command") || error.to_string().contains("oops"),
            "the nested form is refused: {error}"
        );
    }

    #[tokio::test]
    async fn the_rich_declarator_keeps_the_rendered_envelope() {
        let (registry, name) = registered();
        let spec = registry.spec(&name).expect("spec");
        assert_eq!(
            spec.description,
            <ShellTool as norte_core::ToolMetadata>::DESCRIPTION
        );

        let output = registry
            .execute(
                &ToolContext::new(),
                &name,
                serde_json::json!({
                    "script": "echo hi"
                }),
            )
            .await
            .expect("execute");
        assert!(!output.is_error);
        assert!(!output.parts.is_empty(), "a completed call reports output");
        assert!(output.structured.is_some());
    }

    fn registered() -> (norte_core::ToolRegistry, norte_core::ToolName) {
        let mut registry = norte_core::ToolRegistry::new();
        registry
            .register_tool(&ShellTool::new())
            .expect("register command");
        let name = norte_core::ToolName::new("shell").expect("name");
        (registry, name)
    }

    /// A request that runs `script` through the default shell.
    fn request(script: &str) -> ShellRequest {
        ShellRequest::shell(script)
    }

    fn structured(output: &ToolOutput) -> Value {
        output.structured.clone().expect("structured")
    }

    /// Waits until `path` exists, so a test that races a spawned process never
    /// depends on how long the process takes to start.
    async fn wait_for(path: &std::path::Path) {
        for _ in 0..1000 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("timed out waiting for {}", path.display());
    }

    fn temp_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("norte-command-{}-{label}", std::process::id()));
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

    impl norte_core::ProgressSink for Sink {
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

    #[test]
    fn the_shell_defaults_its_interpreter() {
        let request: ShellRequest = serde_json::from_value(serde_json::json!({
            "script": "echo hi"
        }))
        .expect("a script is enough");
        assert_eq!(request.shell, DEFAULT_SHELL);
        assert_eq!(request.script, "echo hi");
    }

    #[test]
    fn the_schema_is_flat_and_the_shell_is_optional() {
        // The point of the tool: no discriminator to forget and no nested
        // `command` object. A model that sends `{"script": "cargo test"}` is
        // always understood.
        let declaration = ShellTool::new().into_declaration();
        let schema = serde_json::to_value(&declaration.parameters).expect("schema");
        let properties = schema["properties"].as_object().expect("properties");
        assert!(
            properties.contains_key("script"),
            "script is a top-level field"
        );
        assert!(
            properties.contains_key("shell"),
            "shell is a top-level field"
        );
        assert!(
            !properties.contains_key("command"),
            "there is no nested command object: {schema}"
        );
        assert!(
            schema.get("oneOf").is_none(),
            "no variant list to guess from: {schema}"
        );
        assert_eq!(properties["shell"]["default"], "sh");
        let required = schema["required"].as_array().expect("required");
        assert!(
            !required.iter().any(|field| field == "shell"),
            "shell stays optional: {schema}"
        );
        assert!(
            required.iter().any(|field| field == "script"),
            "script is required: {schema}"
        );
    }
}
