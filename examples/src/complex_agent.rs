use async_trait::async_trait;
use rustychain::agent::builder::AgentBuilder;
use rustychain::agent::definitions::Agent;
use rustychain::agent::status::AgentStatus;
use rustychain::execution::context::Context;
use rustychain::execution::events::{Event, InterceptionCommand, InterceptionToolCallCommand};
use rustychain::execution::graph::ExecutionGraph;
use rustychain::execution::listeners::EventListener;
use rustychain::prelude::*;
use std::io::{self, Write};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Default, Clone)]
pub struct AgentState {
    tool_audit_log: Vec<String>,
}

// Observer: logs all events
pub struct AuditLogger;

#[async_trait]
impl EventListener<String, ExecutionGraph, AgentState> for AuditLogger {
    async fn on_event(
        &self,
        event: &Event,
        context: &Context<String, ExecutionGraph, AgentState>,
    ) -> Option<InterceptionCommand> {
        match event {
            Event::ToolCallRequested { call, .. } => {
                let mut state = context.state.lock().await;
                state.tool_audit_log.push(call.name.clone());
                println!("📝 Audit: Tool requested - {}", call.name);
            }
            Event::InputInference { inference } => {
                println!(
                    "\n🔍 Inference Input: {}",
                    inference.content.text.as_deref().unwrap_or("")
                );
            }
            Event::InferenceResultReceived { inference, .. } => {
                println!(
                    "\n✅ Inference Result: {}",
                    inference.content.text.as_deref().unwrap_or("")
                );
            }
            Event::FunctionCallsResults { calls, results, .. } => {
                for (call, result) in calls.iter().zip(results.iter()) {
                    println!("\n🔧 Function Call: {} => Result: {:?}", call.name, result);
                }
            }
            Event::StepCompleted { step, .. } => {
                println!("\n✓ Step {} completed", step);
            }
            _ => {}
        }

        None // Observers return None
    }
}

// Interceptor: requires approval for all tools
pub struct ApprovalInterceptor {
    restricted_tools: Vec<String>,
}

#[async_trait]
impl EventListener<String, ExecutionGraph, AgentState> for ApprovalInterceptor {
    async fn on_event(
        &self,
        event: &Event,
        _context: &Context<String, ExecutionGraph, AgentState>,
    ) -> Option<InterceptionCommand> {
        if let Event::ToolCallRequested { call, .. } = event {
            if self.restricted_tools.contains(&call.name) {
                println!("\n[APPROVAL REQUIRED]");
                println!("Tool: {}", call.name);
                println!("Arguments: {}", call.arguments);
                print!("Approve? (y/n/m for modify): ");
                io::stdout().flush().unwrap();

                let mut input = String::new();
                io::stdin().read_line(&mut input).unwrap();

                match input.trim() {
                    "y" => Some(InterceptionCommand::ToolCall(
                        InterceptionToolCallCommand::Continue,
                    )),
                    "m" => {
                        println!("Enter new arguments (JSON):");
                        let mut new_args = String::new();
                        io::stdin().read_line(&mut new_args).unwrap();

                        if let Ok(args) = serde_json::from_str(&new_args) {
                            let mut modified_call = call.clone();
                            modified_call.arguments = args;
                            Some(InterceptionCommand::ToolCall(
                                InterceptionToolCallCommand::Modify {
                                    call: modified_call,
                                },
                            ))
                        } else {
                            println!("Invalid JSON, blocking call");
                            Some(InterceptionCommand::ToolCall(
                                InterceptionToolCallCommand::Block {
                                    reason: "Invalid modified arguments".to_string(),
                                },
                            ))
                        }
                    }
                    _ => Some(InterceptionCommand::ToolCall(
                        InterceptionToolCallCommand::Block {
                            reason: "User denied execution".to_string(),
                        },
                    )),
                }
            } else {
                Some(InterceptionCommand::ToolCall(
                    InterceptionToolCallCommand::Continue,
                ))
            }
        } else {
            None
        }
    }
}

#[tokio::main]
pub async fn main() -> rustychain::Result<()> {
    let llm = build_llm().await?;
    let state = Arc::new(Mutex::new(AgentState::default()));

    let restricted_tools: Vec<String> = llm
        .get_tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();

    let mut agent = build_agent(llm, state.clone(), restricted_tools).await?;

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
    state: Arc<Mutex<AgentState>>,
    restricted_tools: Vec<String>,
) -> rustychain::Result<Agent<String, ExecutionGraph, AgentState>> {
    AgentBuilder::new()
        .name("SecureAgent")
        .llm(llm)
        .state(state)
        .execution_id("session_001".to_string())
        .add_observer(Arc::new(AuditLogger))
        .add_interceptor(Arc::new(ApprovalInterceptor { restricted_tools }), 10)
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
