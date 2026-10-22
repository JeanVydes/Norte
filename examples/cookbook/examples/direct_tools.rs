//! Running the model's tool calls without the engine.
//!
//! The engine's tool step is three things: resolve the name against the
//! registry, call the executor with the arguments the model produced, and turn
//! what comes back — output or failure — into a `ToolResultPart` the next
//! request carries. This recipe does exactly that: `registry.specs()` goes on
//! the request, every `Part::ToolCall` in the answer is dispatched, and the
//! results travel back as one `Message::tool`, in call order.
//!
//! What is deliberately missing is the policy layer the engine adds around the
//! call: deny, rewrite, park for approval, rate-limit, and the cancellation
//! token. Those are host decisions, and this file leaves them out to keep the
//! mechanics visible.
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
    ProviderId, Result as NorteResult, Role, StreamAccumulator, StreamEvent, StreamLimits,
    StreamingGranularity, ToolCallPart, ToolContext, ToolName, ToolOutput, ToolRegistry,
    ToolResultPart, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many requests one conversation may spend before the recipe gives up.
const MAX_STEPS: usize = 4;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = openrouter()?;
    let mut tools = ToolRegistry::new();
    tools.register_tool(&Greet)?;

    let mut transcript = vec![
        Message::system(
            "Use the tools you are given; do not answer from memory. After a tool \
             returns, report its exact text.",
        ),
        Message::user("Call greet for Ada, then reply with the exact text it returned."),
    ];

    for step in 1..=MAX_STEPS {
        println!("\n--- step {step}: request with {} tool(s)", tools.len());
        let answer = turn(&model, &tools, &transcript).await?;
        let calls: Vec<ToolCallPart> = answer.tool_calls().into_iter().cloned().collect();
        transcript.push(answer);
        if calls.is_empty() {
            println!("--- the model answered without a call: the loop ends");
            break;
        }

        // One `Tool` message carries every result of this answer, in call
        // order, exactly as the providers require.
        let mut results = Vec::with_capacity(calls.len());
        for call in &calls {
            println!("\n--- call `{}` with {}", call.name, call.arguments);
            let result = run(&tools, call).await;
            println!(
                "--- result: {} (is_error: {})",
                text(&result),
                result.is_error
            );
            results.push(result);
        }
        transcript.push(Message::tool(results));
    }

    println!("\n--- transcript");
    for message in &transcript {
        let calls = message.tool_calls().len();
        let results = message.tool_results().len();
        if calls > 0 {
            println!("Assistant: {calls} tool call(s)");
        } else if results > 0 {
            println!("Tool: {results} result(s)");
        } else {
            println!("{:?}: {}", message.role, message.text());
        }
    }
    Ok(())
}

/// One request, drained into the single message it produces.
async fn turn(
    model: &dyn Model,
    tools: &ToolRegistry,
    transcript: &[Message],
) -> Result<Message, Box<dyn Error>> {
    let mut request = ModelRequest::new(model.reference().clone(), transcript.to_vec());
    // The tool declarations the model reads: name, description and schema.
    request.tools = tools.specs();
    // A cap keeps the recipe cheap and the answer bounded; without one the
    // vendor charges against the model's full output window.
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
    let accumulated = accumulator.finish()?;
    println!("\n(finish: {:?})", accumulated.finish_reason);
    Ok(Message::new(Role::Assistant, accumulated.parts))
}

/// The whole of tool execution without the engine: resolve the name, call the
/// registry, and make the output — or the failure — the result the model reads.
async fn run(tools: &ToolRegistry, call: &ToolCallPart) -> ToolResultPart {
    let Ok(name) = ToolName::new(call.name.clone()) else {
        return ToolResultPart::error(call.call_id, format!("invalid tool name: {}", call.name));
    };
    let context = ToolContext::new().with_call(call.call_id);
    match tools.execute(&context, &name, call.arguments.clone()).await {
        Ok(output) => ToolResultPart {
            call_id: call.call_id,
            parts: output.parts,
            structured: output.structured,
            is_error: output.is_error,
        },
        // A failed tool is content the model has to see, not an error that ends
        // the loop. The engine makes the same distinction.
        Err(error) => ToolResultPart::error(call.call_id, error.to_string()),
    }
}

/// The text of a result, for the line printed after each call.
fn text(result: &ToolResultPart) -> String {
    result
        .parts
        .iter()
        .filter_map(|part| part.as_text())
        .collect::<Vec<_>>()
        .join(" ")
}

// ── the tool ────────────────────────────────────────────────────────────────

/// Greets one person. A rich tool returns [`ToolOutput`], so it controls the
/// text, the structured value and the error flag the model receives.
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
    async fn call(&self, args: GreetArgs) -> NorteResult<ToolOutput> {
        Ok(ToolOutput::text(format!("Hello, {}!", args.name)))
    }
}

// ── the wiring ──────────────────────────────────────────────────────────────

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter() -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities(),
    )?)
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(norte_core::ToolKind::Function)
        .build()
}
