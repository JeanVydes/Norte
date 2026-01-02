use std::{pin::Pin, sync::Arc};

use rustychain::{FnDeclarator, FnExecutor, FunctionDeclaration};
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};

use crate::os::{
    CommandArgs, CommandExecutor, CommandResult, InputAction, InputCallback, InputContext,
    InputDetector, SecurityPolicy,
};

#[derive(Clone)]
pub struct CommandTool {
    pub security_policy: Arc<SecurityPolicy>,
    pub executor: Arc<CommandExecutor>,
}

impl CommandTool {
    pub fn new() -> Self {
        Self::with_security_policy(SecurityPolicy::default())
    }

    pub fn with_security_policy(policy: SecurityPolicy) -> Self {
        log::warn!("⚠️  CommandTool initialized - allows OS command execution");
        log::warn!(
            "⚠️  Current policy: shell={}, audit={}",
            policy.allow_shell,
            policy.audit_logging
        );

        let detector = InputDetector::new(vec![]);
        let executor = CommandExecutor::new(detector, None);

        Self {
            security_policy: Arc::new(policy),
            executor: Arc::new(executor),
        }
    }

    pub fn with_input_callback<F, Fut>(mut self, callback: F) -> Self
    where
        F: Fn(InputContext) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = rustychain::Result<InputAction>> + Send + 'static,
    {
        let callback_arc: Arc<InputCallback> = Arc::new(Box::new(move |ctx: InputContext| {
            let fut = callback(ctx);
            Box::pin(fut)
                as Pin<
                    Box<dyn std::future::Future<Output = rustychain::Result<InputAction>> + Send>,
                >
        }));

        // Create new executor with callback
        let detector = self.executor.input_detector.clone();
        self.executor = Arc::new(CommandExecutor::new(detector, Some(callback_arc)));
        self
    }

    fn validate_args(&self, args: &CommandArgs) -> rustychain::Result<()> {
        // Check command allowed
        self.security_policy
            .is_command_allowed(&args.command)
            .map_err(rustychain::Error::Generic)?;

        // Check directory allowed
        if let Some(ref dir) = args.working_dir {
            self.security_policy
                .is_directory_allowed(dir)
                .map_err(rustychain::Error::Generic)?;
        }

        // Check shell usage
        if args.use_shell && !self.security_policy.allow_shell {
            return Err(rustychain::Error::Generic(
                "Shell execution is disabled by security policy".into(),
            ));
        }

        // Check timeout limits
        if args.timeout_secs > self.security_policy.max_timeout_secs {
            return Err(rustychain::Error::Generic(format!(
                "Timeout exceeds maximum allowed: {} > {}",
                args.timeout_secs, self.security_policy.max_timeout_secs
            )));
        }

        // Check destructive commands
        if self.security_policy.require_approval_for_destructive
            && self
                .security_policy
                .is_potentially_destructive(&args.command, &args.args)
        {
            log::warn!(
                "⚠️  Potentially destructive command: {} {:?}",
                args.command,
                args.args
            );
            todo!("Implement approval workflow for destructive commands");
        }

        Ok(())
    }

    fn audit_log(&self, args: &CommandArgs, result: &CommandResult) {
        if !self.security_policy.audit_logging {
            return;
        }

        log::debug!(
            "AUDIT: cmd={} args={:?} exit_code={:?} duration={}ms timed_out={} killed={}",
            args.command,
            args.args,
            result.exit_code,
            result.duration_ms,
            result.timed_out,
            result.killed
        );
    }
}

impl Default for CommandTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl FnExecutor<CommandArgs, serde_json::Value> for CommandTool {
    async fn call(&self, args: CommandArgs) -> rustychain::Result<serde_json::Value> {
        // Validate against security policy
        self.validate_args(&args)?;

        // Handle background execution
        if args.background {
            let mut cmd = self.executor.build_command(&args);
            let child = cmd.spawn()?;
            let pid = child.id();
            
            return Ok(serde_json::json!({
                "status": "background",
                "pid": pid,
                "message": format!("Command started in background with PID {:?}", pid)
            }));
        }

        // Execute command
        let result = self.executor.execute(args.clone()).await?;

        // Audit log
        self.audit_log(&args, &result);

        // Return structured result
        let status = if result.timed_out {
            "timeout"
        } else if result.killed {
            "cancelled"
        } else if result.exit_code == Some(0) {
            "success"
        } else {
            "error"
        };

        Ok(serde_json::json!({
            "status": status,
            "stdout": result.stdout,
            "stderr": result.stderr,
            "exit_code": result.exit_code,
            "duration_ms": result.duration_ms,
            "timed_out": result.timed_out,
            "killed": result.killed,
            "message": result.cancel_reason
        }))
    }
}

impl FnDeclarator<CommandArgs, serde_json::Value> for CommandTool {
    fn declare(&self) -> FunctionDeclaration<CommandArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "command_tool",
            description: "Execute OS commands with full interactive support. ⚠️ Use with caution - respects security policies.",
            parameters: schema_for!(CommandArgs),
            executor: Arc::new(self.clone()),
        }
    }
}

// ============================================================================
// SIGNAL TOOL
// ============================================================================

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct SignalArgs {
    #[schemars(description = "Process ID to send the signal to.")]
    pub pid: u32,
    #[schemars(
        description = "Signal: 'kill', 'term', 'int', 'stop', 'cont', 'hup', 'usr1', 'usr2'."
    )]
    pub signal: String,
}

#[derive(Clone, Default)]
pub struct SignalTool;

#[async_trait::async_trait]
impl FnExecutor<SignalArgs, serde_json::Value> for SignalTool {
    async fn call(&self, args: SignalArgs) -> rustychain::Result<serde_json::Value> {
        #[cfg(unix)]
        {
            let signal_num = match args.signal.to_lowercase().as_str() {
                "kill" | "sigkill" | "9" => "9",
                "term" | "sigterm" | "15" => "15",
                "int" | "sigint" | "2" => "2",
                "stop" | "sigstop" | "19" => "19",
                "cont" | "sigcont" | "18" => "18",
                "hup" | "sighup" | "1" => "1",
                "usr1" | "sigusr1" | "10" => "10",
                "usr2" | "sigusr2" | "12" => "12",
                _ => {
                    return Err(rustychain::Error::Generic(format!(
                        "Unknown signal: {}",
                        args.signal
                    )));
                }
            };

            let output = std::process::Command::new("kill")
                .arg(format!("-{}", signal_num))
                .arg(args.pid.to_string())
                .output()?;

            if output.status.success() {
                Ok(serde_json::json!({
                    "status": "success",
                    "message": format!("Sent {} to process {}", args.signal, args.pid)
                }))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(rustychain::Error::Generic(format!(
                    "Failed to send signal {} to PID {}: {}",
                    args.signal, args.pid, stderr
                )))
            }
        }

        #[cfg(not(unix))]
        Err(rustychain::Error::Generic(
            "Signal sending is only supported on Unix systems".into(),
        ))
    }
}

impl FnDeclarator<SignalArgs, serde_json::Value> for SignalTool {
    fn declare(&self) -> FunctionDeclaration<SignalArgs, serde_json::Value> {
        FunctionDeclaration {
            name: "signal_tool",
            description: "Send signals to processes by PID (terminate, interrupt, etc.).",
            parameters: schema_for!(SignalArgs),
            executor: Arc::new(Self),
        }
    }
}
