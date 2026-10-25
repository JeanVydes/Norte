//! A policy that decides on what the request already did.
//!
//! `PolicyView` hands a policy two extra views of the request in flight:
//! `results`, the results the same assistant message already produced, in call
//! order, and `history`, the messages of this request.
//!
//! `results` is a *sequential* concept. With `HarnessConfig::early_tool_execution`
//! on, a call is dispatched in parallel with the rest of the stream, so its policy
//! is evaluated before anything has run and `results` arrives empty; a call the
//! policy defers is evaluated later and sees whatever already ran. A policy that
//! depends on `results` must leave early execution off, which is the default.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example policy_context
//! ```

use std::error::Error;
use std::sync::Arc;

use norte_core::{
    Capabilities, FnExecutor, Message, Modality, ModelRef, Role, StreamingGranularity, ToolKind,
    ToolRegistry, WireFamily,
};
use norte_harness::{
    Decision, HarnessBuilder, InMemoryBlobStore, InMemoryStore, Policy, PolicySet, PolicyView,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const SYSTEM: &str = "You delete files when asked. If asked to delete two files, call the \
                      `delete_file` tool once per file, both in the same message.";

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

/// Prints what it sees, and allows only the first call of a message.
struct OnePerMessage;

impl Policy for OnePerMessage {
    fn name(&self) -> &'static str {
        "one-per-message"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        println!(
            "  policy sees {} earlier result(s) in this message, {} message(s) in this request",
            view.results.len(),
            view.history.len(),
        );
        if view.results.is_empty() {
            Decision::Allow
        } else {
            Decision::Deny("this message already ran a tool".to_owned())
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&DeleteFile)?;
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
        .policies(PolicySet::new(vec![Arc::new(OnePerMessage)]))
        .build()?;

    let session = harness.create().await?;
    harness
        .append_history(session.id, vec![Message::system(SYSTEM)])
        .await?;
    let outcome = harness
        .send(
            session.id,
            Message::user("Delete sandbox/a.txt and sandbox/b.txt."),
        )
        .finish()
        .await?;

    // With one call in the message the first is always allowed: to see the
    // refusal, the model has to emit both calls in the same message.
    let mut ran = 0_u32;
    let mut refused = 0_u32;
    for message in harness.history(session.id).await? {
        if message.role != Role::Tool {
            continue;
        }
        for result in message.tool_results() {
            if result.is_error {
                refused += 1;
            } else {
                ran += 1;
            }
        }
    }
    println!("{ran} call(s) ran, {refused} refused");
    println!(
        "answer: {}",
        outcome
            .message
            .as_ref()
            .map(Message::text)
            .unwrap_or_default()
    );
    Ok(())
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
