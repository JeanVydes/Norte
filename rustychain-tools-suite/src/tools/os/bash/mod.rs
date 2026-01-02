pub mod args;
pub mod executor;
pub mod input;
pub mod policy;
pub mod tool;

pub use args::*;
pub use executor::*;
pub use input::*;
pub use policy::*;
pub use tool::*;

use tokio::io::AsyncWriteExt;

async fn get_user_input_from_terminal(prompt: &str) -> rustychain::Result<String> {
    print!("\n🔒 Input required: {}\n> ", prompt.trim());
    tokio::io::stdout().flush().await?;

    tokio::task::spawn_blocking(|| {
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        Ok(input)
    })
    .await?
}

#[cfg(test)]
mod tests {
    use crate::os::{args::CommandArgs, policy::SecurityPolicy, tool::CommandTool};

    #[test]
    fn test_security_policy_blocked_commands() {
        let policy = SecurityPolicy::default();
        assert!(policy.is_command_allowed("rm").is_err());
        assert!(policy.is_command_allowed("ls").is_ok());
    }

    #[test]
    fn test_security_policy_whitelist() {
        let mut policy = SecurityPolicy::default();
        policy.allowed_commands = ["ls", "pwd"].iter().map(|s| s.to_string()).collect();

        assert!(policy.is_command_allowed("ls").is_ok());
        assert!(policy.is_command_allowed("pwd").is_ok());
        assert!(policy.is_command_allowed("cat").is_err());
    }

    #[test]
    fn test_destructive_detection() {
        let policy = SecurityPolicy::default();
        assert!(policy.is_potentially_destructive("rm", &["-rf".into(), "/tmp".into()]));
        assert!(!policy.is_potentially_destructive("ls", &["-la".into()]));
    }

    #[tokio::test]
    async fn test_simple_command() {
        let tool = CommandTool::new();
        let args = CommandArgs {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            working_dir: None,
            env: vec![],
            timeout_secs: 10,
            background: false,
            use_shell: false,
            custom_input_patterns: vec![],
        };

        let result = tool.executor.execute(args).await.unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(result.stdout.contains("hello"));
    }
}
