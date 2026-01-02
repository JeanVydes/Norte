use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct EnvVar {
    #[schemars(description = "The environment variable name.")]
    pub name: String,
    #[schemars(description = "The environment variable value.")]
    pub value: String,
}

#[derive(JsonSchema, Serialize, Deserialize, Debug, Clone)]
pub struct CommandArgs {
    #[schemars(description = "The command to execute (e.g., 'ls', 'git', 'cargo').")]
    pub command: String,

    #[schemars(description = "Arguments to pass to the command.")]
    #[serde(default)]
    pub args: Vec<String>,

    #[schemars(description = "Working directory. If not provided, uses current directory.")]
    #[serde(default)]
    pub working_dir: Option<String>,

    #[schemars(description = "Environment variables as name-value pairs.")]
    #[serde(default)]
    pub env: Vec<EnvVar>,

    #[schemars(
        description = "Timeout in seconds. Default is 300 (5 minutes). Set to 0 for no timeout."
    )]
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,

    #[schemars(description = "Run in background. Returns immediately with a process ID.")]
    #[serde(default)]
    pub background: bool,

    #[schemars(
        description = "Use shell (allows pipes, redirects). SECURITY RISK: Only enable if necessary."
    )]
    #[serde(default)]
    pub use_shell: bool,

    #[schemars(description = "Custom input patterns to detect.")]
    #[serde(default)]
    pub custom_input_patterns: Vec<String>,
}

fn default_timeout() -> u64 {
    300
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub killed: bool,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_reason: Option<String>,
}
