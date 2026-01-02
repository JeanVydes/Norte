use rustychain::agent::builder::AgentBuilder;
use rustychain::agent::definitions::{Agent};
use rustychain::agent::status::AgentStatus;
use rustychain::execution::graph::ExecutionGraph;
use rustychain::prelude::*;
use std::io::{self, Write};
use std::sync::Arc;

#[derive(Default, Clone)]
pub struct AgentState {}

#[tokio::main]
pub async fn main() -> rustychain::Result<()> {
    let mut agent = build_agent(build_llm().await?).await?;
    run_chat_loop(&mut agent).await
}

pub async fn build_llm() -> rustychain::Result<Arc<LLM>> {
    Ok(Arc::new(
        LLM::builder()
            .set_authorization("your_api_key_here")
            .set_model("gemini-2.5-flash")
            .set_provider(LLMProvider::Google)
            .set_system_prompt("You are a helpful agent. Use tools when needed.")
            .add_tool(
                rustychain_tools_suite::integrations::search::DuckDuckGoSearchTool::new().declare(),
            )
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
