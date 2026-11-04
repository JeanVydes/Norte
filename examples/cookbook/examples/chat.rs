//! A whole conversation in one program: history and tools.
//!
//! The `tool_loop` recipe shows the loop for one user message. This one is
//! the program a host actually ships, minus the UI: several user turns over
//! one history, every tool call executed, and the assistant's answer
//! printed back. No key and no network — the model is scripted.
//!
//! The shape to copy: `run_turn` is the only function that knows the loop.
//! Everything else is wiring the caller already wrote.
//!
//! ```sh
//! cargo run -p norte-cookbook --example chat
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_cookbook::weather_tool;
use norte_core::{
    CallId, Message, Model, ModelRequest, Part, Role, StreamAccumulator, StreamLimits,
    ToolRegistry, ToolResultPart,
};
use norte_testing::ScriptedModel;
use serde_json::json;

/// One model round trip over `history`, executing every tool call it asks
/// for. On success the assistant message and its tool results are in
/// `history`; on failure nothing is appended, so a retry starts clean.
async fn run_turn(
    model: &ScriptedModel,
    registry: &ToolRegistry,
    history: &mut Vec<Message>,
) -> Result<String, Box<dyn Error>> {
    let mut request = ModelRequest::new(model.reference().clone(), history.clone());
    request.tools = registry.specs();

    let mut stream = model.stream(request).await?;
    let mut accumulator = StreamAccumulator::new(StreamLimits::default());
    while let Some(event) = stream.next().await {
        accumulator.apply(event?)?;
    }
    let finished = accumulator.finish()?;

    let calls: Vec<_> = finished
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::ToolCall(call) => Some(call),
            _ => None,
        })
        .collect();

    let answer = Message::new(Role::Assistant, finished.parts.clone());
    history.push(answer.clone());
    for call in &calls {
        let output =
            weather_tool::execute(registry, &call.name, call.call_id, &call.arguments).await?;
        history.push(Message::new(
            Role::Tool,
            vec![Part::ToolResult(ToolResultPart {
                call_id: call.call_id,
                parts: output.parts,
                structured: output.structured,
                is_error: output.is_error,
            })],
        ));
    }
    Ok(answer.text())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut registry = ToolRegistry::new();
    registry.register_tool_declaration(weather_tool::Weather::declaration())?;

    // Question 1 takes two requests (tool call, then the answer).
    // Question 2 takes one. Three scripted turns, in order.
    let model = ScriptedModel::new(norte_testing::model("test/scripted"))
        .turn_tool_call(CallId::new(), "weather", &json!({ "city": "Lisbon" }))
        .turn_text("It is 22 degrees and clear in Lisbon.")
        .turn_text("And 18 in Porto, cloudy.");

    let mut history = vec![Message::system("You answer with weather.")];

    for question in ["What is the weather in Lisbon?", "And in Porto?"] {
        history.push(Message::user(question));
        // A question may take several requests: run turns until the model
        // stops asking for tools, then print its final answer.
        loop {
            let answer = run_turn(&model, &registry, &mut history).await?;
            if answer.is_empty() {
                continue;
            }
            println!("assistant: {answer}");
            break;
        }
    }

    println!("--- {} request(s) made", model.request_count());
    Ok(())
}
