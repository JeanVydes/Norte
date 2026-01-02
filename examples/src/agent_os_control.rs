use rustychain::agent::builder::AgentBuilder;
use rustychain::agent::definitions::Agent;
use rustychain::agent::status::AgentStatus;
use rustychain::execution::graph::ExecutionGraph;
use rustychain::prelude::*;
use rustychain_tools_suite::os::{FileSystemConfig, SecurityPolicy, all_filesystem_tools};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct AgentState {}

#[tokio::main]
pub async fn main() -> rustychain::Result<()> {
    let mut agent = build_agent(build_llm().await?).await?;
    run_chat_loop(&mut agent).await
}

pub async fn build_llm() -> rustychain::Result<Arc<LLM>> {
    // Define a root directory for filesystem access, this is in this way for security, not full access to whatever path, just your designated dir
    let temp_dir: PathBuf = "/home/username/test_directory".into();
    // Set up filesystem config to restrict access to the temp_dir
    let fs_config = Arc::new(FileSystemConfig::default().set_allowed_root(temp_dir.clone()));
    // Set up security policy for command execution, THIS IS VERY IMPORTANT FOR SECURITY
    let cmd_policy = SecurityPolicy::default()
        .add_allowed_directory(temp_dir.clone())
        //.add_blocked_command("")
        .enable_audit_logging(true)
        .set_max_timeout(60)
        // allow shell execution like pipes and redirects (careful with security!) this can be used incorrectly and allow not desired access and actions
        .allow_shell_execution(true);

    Ok(Arc::new(
        LLM::builder()
            .set_authorization("your_api_key_here")
            .set_model("mistralai/devstral-2512:free")
            .set_provider(LLMProvider::OpenRouter)
            .set_system_prompt("You are a helpful agent. Use tools when needed.")
            .add_tool(
                rustychain_tools_suite::os::bash::CommandTool::with_security_policy(
                    cmd_policy.clone(),
                )
                .declare(),
            )
            // this is a helper to add all filesystem related tools with the given config
            // but, you can provide granularly the ones you want too
            .add_tools(all_filesystem_tools(fs_config))
            .build()?,
    ))
}

pub async fn build_agent(
    llm: Arc<LLM>,
) -> rustychain::Result<Agent<String, ExecutionGraph, AgentState>> {
    AgentBuilder::new()
        .name("Agent")
        .llm(llm)
        .execution_id("session_001".to_string())
        .build()
        .await
}

pub async fn run_chat_loop(
    agent: &mut Agent<String, ExecutionGraph, AgentState>,
) -> rustychain::Result<()> {
    println!("🤖 Agent ready. Type 'exit' to quit, 'clear' to reset.");

    loop {
        let user_input = read_user_input()?;

        match user_input.as_str() {
            "exit" => break,
            "clear" => {
                agent.hard_reset().await?;
                println!("✓ History cleared.");
                continue;
            }
            "" => continue,
            input => {
                if let Err(e) = process_user_message(agent, input).await {
                    println!("❌ Error: {}", e);
                }
            }
        }
    }
    Ok(())
}

pub fn read_user_input() -> rustychain::Result<String> {
    print!("\n👤 You > ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

pub async fn process_user_message(
    agent: &mut Agent<String, ExecutionGraph, AgentState>,
    message: &str,
) -> rustychain::Result<()> {
    let mut status = agent.next_step(Some(Inference::as_user(message))).await?;

    loop {
        match status {
            AgentStatus::Executing => {
                status = agent.next_step(None).await?;
            }

            AgentStatus::Finished(inference) => {
                if let Some(text) = inference.content.text {
                    println!("\n🤖 Agent > {}", text);
                }
                break;
            }

            AgentStatus::Faulted(error) => {
                println!("❌ Agent faulted: {}", error);
                break;
            }

            _ => break,
        }
    }

    Ok(())
}
