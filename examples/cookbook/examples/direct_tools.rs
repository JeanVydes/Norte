//! Running the model's tool calls without the engine.
//!
//! `request.tools` carries the declarations; every call in the answer is
//! dispatched through a `ToolRegistry`, and the results go back as one
//! `Message::tool`, in call order. The loop ends when the answer has no call.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... \
//! COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
//! COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
//!   cargo run -p norte-cookbook --example direct_tools
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_core::{
    Capabilities, FnExecutor, Message, Modality, Model, ModelId, ModelRef, ModelRequest,
    ProviderId, Role, StreamAccumulator, StreamEvent, StreamLimits, StreamingGranularity,
    ToolCallPart, ToolContext, ToolKind, ToolName, ToolOutput, ToolRegistry, ToolResultPart,
    WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The engine's default cap on requests per turn, kept here so a model that
/// never stops calling tools cannot run away with the budget.
const MAX_STEPS: usize = 16;

#[norte_core::tool(name = "greet", description = "Greet one person by name")]
struct Greet;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GreetArgs {
    /// The person to greet.
    name: String,
}

#[async_trait::async_trait]
impl FnExecutor<GreetArgs, ToolOutput> for Greet {
    async fn call(&self, args: GreetArgs) -> norte_core::Result<ToolOutput> {
        Ok(ToolOutput::text(format!("Hello, {}!", args.name)))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(
        provider.clone(),
        WireFamily::new(WIRE_CHAT)?,
        &std::env::var("COOKBOOK_ENDPOINT")?,
    )?
    .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let model = OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&std::env::var("COOKBOOK_MODEL")?)?),
        profile,
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .tool_kind(ToolKind::Function)
            .build(),
    )?;

    let mut tools = ToolRegistry::new();
    tools.register_tool(&Greet)?;

    let mut history = vec![
        Message::system("Use the tools; do not answer from memory."),
        Message::user("Greet Ada."),
    ];

    for _ in 0..MAX_STEPS {
        let mut request = ModelRequest::new(model.reference().clone(), history.clone());
        request.tools = tools.specs();
        request.params.max_output_tokens = Some(1024);

        let mut stream = model.stream(request).await?;
        let mut accumulator = StreamAccumulator::new(StreamLimits::default());
        while let Some(event) = stream.next().await {
            let event = event?;
            if let StreamEvent::TextDelta { text, .. } = &event {
                print!("{text}");
            }
            accumulator.apply(event)?;
        }

        let answer = Message::new(Role::Assistant, accumulator.finish()?.parts);
        let calls: Vec<_> = answer.tool_calls().into_iter().cloned().collect();
        history.push(answer);
        if calls.is_empty() {
            break;
        }

        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            println!("-> {}({})", call.name, call.arguments);
            results.push(
                execute_call(&tools, call)
                    .await
                    .unwrap_or_else(|error| ToolResultPart::error(call.call_id, error.to_string())),
            );
        }
        history.push(Message::tool(results));
    }
    Ok(())
}

async fn execute_call(
    tools: &ToolRegistry,
    call: &ToolCallPart,
) -> Result<ToolResultPart, Box<dyn Error>> {
    let output = tools
        .execute(
            &ToolContext::new(),
            &ToolName::new(call.name.clone())?,
            call.arguments.clone(),
        )
        .await?;

    Ok(ToolResultPart {
        call_id: call.call_id,
        structured: output.structured,
        parts: output.parts,
        is_error: output.is_error,
    })
}
