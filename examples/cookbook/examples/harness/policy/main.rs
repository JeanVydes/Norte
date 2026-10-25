//! Policies: what happens to a tool call before it runs.
//!
//! A policy returns one of five `Decision`s: `Allow`, `Deny(reason)`,
//! `RequireApproval(reason)`, `Transform(arguments)` and `RateLimit(retry_after)`.
//! A `PolicySet` folds them with a fixed precedence — a `Deny` from any policy
//! wins, then approval, then argument rewriting, then rate limiting — so a policy
//! added to the set can never weaken the ones already there.
//!
//! Both runs use the same tool: `delete_file`, which only echoes the path it was
//! asked to delete, so the recipe can print what the engine really ran. Each run
//! prints the call as the *model* made it, then every result the engine produced
//! for it — and says so when the model answered without calling at all, since
//! then no policy ran.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example policy
//! ```

use std::error::Error;
use std::sync::Arc;

use norte_core::{
    Capabilities, FnExecutor, Message, Modality, ModelRef, Part, StreamingGranularity, ToolKind,
    ToolRegistry, ToolResultPart, WireFamily,
};
use norte_harness::{
    Decision, Harness, HarnessBuilder, InMemoryBlobStore, InMemoryStore, Policy, PolicySet,
    PolicyView,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SYSTEM: &str = "You delete files when asked. Call the `delete_file` tool with the exact \
                      path you were given, once, and do not ask for confirmation: the host's \
                      policies decide what is allowed. Then report what the tool result says: if \
                      the call failed, say that it failed and why; never claim a file was deleted \
                      when the tool refused.";

#[norte_core::tool(name = "delete_file", description = "Delete a file")]
struct DeleteFile;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteFileArgs {
    path: String,
}

#[derive(Debug, Serialize)]
struct DeleteFileResult {
    deleted: String,
}

#[async_trait::async_trait]
impl FnExecutor<DeleteFileArgs, DeleteFileResult> for DeleteFile {
    async fn call(&self, args: DeleteFileArgs) -> norte_core::Result<DeleteFileResult> {
        Ok(DeleteFileResult { deleted: args.path })
    }
}

/// Keeps the agent out of the host's secret store: a host rule, not the model's
/// judgement.
struct ProtectSecrets;

impl Policy for ProtectSecrets {
    fn name(&self) -> &'static str {
        "protect-secrets"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        match path_of(view) {
            Some(path) if path.starts_with("secrets/") => {
                Decision::Deny(format!("{path} is in the secret store"))
            }
            _ => Decision::Allow,
        }
    }
}

/// Re-roots every other path inside the sandbox, before the call runs.
struct IntoSandbox;

impl Policy for IntoSandbox {
    fn name(&self) -> &'static str {
        "into-sandbox"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        match path_of(view) {
            Some(path) if !path.starts_with("sandbox/") => Decision::Transform(serde_json::json!({
                "path": format!("sandbox/{}", path.trim_start_matches('/')),
            })),
            _ => Decision::Allow,
        }
    }
}

fn path_of<'b>(view: &PolicyView<'b>) -> Option<&'b str> {
    view.call.arguments.get("path").and_then(Value::as_str)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&DeleteFile)?;
    let policies = PolicySet::new(vec![Arc::new(ProtectSecrets), Arc::new(IntoSandbox)]);
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build();
    let model = openrouter(capabilities)?;
    let harness = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .policies(policies)
        .build()?;

    println!("--- one policy denies the call, and the model gets the refusal back");
    ask(&harness, "Delete secrets/notes.txt.").await?;

    println!("\n--- another policy rewrites the call before the tool runs");
    ask(&harness, "Delete /tmp/notes.txt.").await?;
    Ok(())
}

/// One turn: the calls the model asked for, what the engine did with them, and
/// the answer.
async fn ask(harness: &Harness, prompt: &str) -> Result<(), Box<dyn Error>> {
    let session = harness.create().await?;
    harness
        .append_history(session.id, vec![Message::system(SYSTEM)])
        .await?;
    let outcome = harness
        .send(session.id, Message::user(prompt))
        .finish()
        .await?;

    let mut calls = 0_usize;
    for message in harness.history(session.id).await? {
        for part in &message.parts {
            if let Some(call) = part.as_tool_call() {
                println!("call: {}({})", call.name, call.arguments);
                calls += 1;
            }
        }
        for result in message.tool_results() {
            print_result(result);
        }
    }
    if calls == 0 {
        println!("no call: the model answered without calling a tool, so no policy ran");
    }
    println!("answer: {}", answer(outcome.message.as_ref()));
    Ok(())
}

fn print_result(result: &ToolResultPart) {
    let status = if result.is_error { "failed" } else { "ok" };
    println!("tool result ({status}): {}", result_text(result));
}

fn result_text(result: &ToolResultPart) -> &str {
    result
        .parts
        .iter()
        .find_map(Part::as_text)
        .unwrap_or_default()
}

fn answer(message: Option<&Message>) -> String {
    message.map(Message::text).unwrap_or_default()
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
