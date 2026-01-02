use std::pin::Pin;

/// Patterns that indicate the command is waiting for user input
pub const INPUT_PATTERNS: &[&str] = &[
    "password:",
    "passphrase:",
    "[y/n]",
    "(y/n)",
    "yes/no",
    "[yes/no]",
    "(yes/no)",
    "continue?",
    "proceed?",
    "confirm",
    "enter",
    "input:",
    "prompt:",
    "username:",
    "user:",
    "login:",
    "token:",
    "api key:",
    "secret:",
    "[sudo]",
    "are you sure",
    "overwrite",
    "replace",
    "delete",
    "(y/n/a)",
    "[y/n/a]",
];

/// Action to take when a command requests input
#[derive(Debug, Clone)]
pub enum InputAction {
    Provide(String),
    Cancel,
    Skip,
    Interrupt,
    SendEof,
}

/// Context provided to the input callback
#[derive(Debug, Clone)]
pub struct InputContext {
    pub prompt: String,
    pub stdout_so_far: String,
    pub stderr_so_far: String,
    pub command: String,
    pub args: Vec<String>,
    pub elapsed_ms: u64,
    pub matched_pattern: Option<String>,
}

/// Input pattern detector
#[derive(Clone)]
pub struct InputDetector {
    pub additional_patterns: Vec<String>,
}

impl InputDetector {
    pub fn new(additional_patterns: Vec<String>) -> Self {
        Self {
            additional_patterns,
        }
    }

    pub fn find_pattern(&self, line: &str, custom_patterns: &[String]) -> Option<String> {
        let lower = line.to_lowercase();

        // Check default patterns
        for pattern in INPUT_PATTERNS {
            if lower.contains(pattern) {
                return Some(pattern.to_string());
            }
        }

        // Check additional patterns
        for pattern in &self.additional_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                return Some(pattern.clone());
            }
        }

        // Check custom patterns from args
        for pattern in custom_patterns {
            if lower.contains(&pattern.to_lowercase()) {
                return Some(pattern.clone());
            }
        }

        None
    }

    pub fn needs_input(&self, line: &str, custom_patterns: &[String]) -> bool {
        self.find_pattern(line, custom_patterns).is_some()
    }
}

/// Callback type for input handling
pub type InputCallback = Box<
    dyn Fn(
            InputContext,
        )
            -> Pin<Box<dyn std::future::Future<Output = rustychain::Result<InputAction>> + Send>>
        + Send
        + Sync,
>;
