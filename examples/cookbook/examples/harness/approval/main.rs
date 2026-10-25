//! Human approval: park the turn, decide, resume.
//!
//! A policy that answers `RequireApproval` does not run the call: the turn parks
//! with `Phase::AwaitingApproval`. The park is a commit, so the decision can come
//! later, from another process, after a restart — nothing is held in memory and
//! no call is ever run twice.
//!
//! What happens to the *other* calls of the same message: the ones before the
//! gated call have already run, and their results are persisted with the park so
//! they are not repeated; the ones after it are neither run nor even evaluated
//! until the host decides. With `early_tool_execution` on, the siblings were
//! already started during the stream, so they run before the human answers.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example approval
//! ```

use std::error::Error;
use std::sync::Arc;

use norte_core::{
    Capabilities, FnExecutor, GrantScope, Message, Modality, ModelRef, Role, SessionId,
    StreamingGranularity, ToolKind, ToolName, ToolRegistry, WireFamily,
};
use norte_harness::{
    Harness, HarnessBuilder, InMemoryBlobStore, InMemoryStore, PolicySet, ToolApprovalPolicy,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const SYSTEM: &str =
    "You deploy services. When asked to deploy, call the `deploy` tool instead of answering.";

/// The one tool this recipe gates.
#[norte_core::tool(name = "deploy", description = "Deploy a service to an environment")]
struct Deploy;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeployArgs {
    service: String,
    environment: String,
}

#[derive(Debug, Serialize)]
struct DeployResult {
    service: String,
    environment: String,
    url: String,
}

#[async_trait::async_trait]
impl FnExecutor<DeployArgs, DeployResult> for Deploy {
    async fn call(&self, args: DeployArgs) -> norte_core::Result<DeployResult> {
        Ok(DeployResult {
            url: format!("https://{}.example.test", args.environment),
            service: args.service,
            environment: args.environment,
        })
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Deploy)?;
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "human",
        vec![ToolName::new("deploy")?],
        "deploying touches production",
    ))]);
    let harness = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(openrouter(tool_capabilities())?))
        .tools(tools)
        .policies(policies)
        .build()?;

    let session = harness.create().await?;
    harness
        .append_history(session.id, vec![Message::system(SYSTEM)])
        .await?;

    // The turn stops at the gated call: the model already answered, the tool did
    // not run, and the session sits on a durable record.
    let outcome = harness
        .send(session.id, Message::user("Deploy checkout to staging."))
        .finish()
        .await?;
    println!("phase: {:?}", outcome.session.phase);

    for record in harness.pending_approvals(session.id).await? {
        println!("waiting on {} (approval {})", record.tool, record.id);
        // `GrantScope::Call` authorizes this call, and only this one. `Tool`,
        // `Session` and `Global` would authorize later ones without asking.
        harness
            .approve(record.id, GrantScope::Call(record.call_id), "operator")
            .await?;
    }

    // Resuming does not ask the model again: it runs the call it was waiting for
    // and carries on from there.
    let outcome = harness.resume(session.id).finish().await?;
    println!("phase: {:?}", outcome.session.phase);
    println!("answer: {}", answer(outcome.message.as_ref()));
    print_tool_results(&harness, session.id).await
}

async fn print_tool_results(harness: &Harness, session: SessionId) -> Result<(), Box<dyn Error>> {
    for message in harness.history(session).await? {
        if message.role != Role::Tool {
            continue;
        }
        for result in message.tool_results() {
            let status = if result.is_error { "failed" } else { "ok" };
            println!("tool result ({status})");
            if let Some(structured) = &result.structured {
                println!("  structured: {structured}");
            }
        }
    }
    Ok(())
}

fn answer(message: Option<&Message>) -> String {
    message.map(Message::text).unwrap_or_default()
}

fn tool_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    Ok(OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?
                .as_str()
                .try_into()?,
        ),
        VendorProfile::new(
            "openrouter".try_into()?,
            WireFamily::new(WIRE_CHAT)?,
            &required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?,
        )?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?)),
        capabilities,
    )?)
}
