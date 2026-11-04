//! A complete tool loop with no engine and no session.
//!
//! This is the recipe a host copies when it writes its own loop: send the
//! history, stream the answer, run every tool call the model asked for,
//! append the results and send again — until the model stops asking. The
//! library hands you the hard parts: `Model` streams neutral events,
//! `StreamAccumulator` folds them into `Part`s, the registry owns the
//! declarations and executes the calls. What the host owns is the history,
//! the loop and the decision of when to stop.
//!
//! No key and no network: the model is scripted. Turn 1 asks for a tool
//! call, turn 2 answers with text. Swap the scripted model for a preset
//! (`norte::presets::openai`) and the same loop runs against a provider.
//!
//! ```sh
//! cargo run -p norte-cookbook --example tool_loop
//! ```

use std::error::Error;

use futures_util::StreamExt;
use norte_cookbook::weather_tool;
use norte_core::{
    CallId, Message, Model, ModelRequest, Part, Role, StreamAccumulator, StreamEvent, StreamLimits,
    ToolRegistry, ToolResultPart,
};
use norte_testing::ScriptedModel;
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // The registry the model sees: declarations plus executors.
    let mut registry = ToolRegistry::new();
    registry.register_tool_declaration(weather_tool::Weather::declaration())?;

    // A scripted model: turn 1 calls the tool, turn 2 answers.
    let model = ScriptedModel::new(norte_testing::model("test/scripted"))
    .turn_tool_call(CallId::new(), "weather", &json!({ "city": "Lisbon" }))
    .turn_text("It is 22 degrees and clear in Lisbon.");

    let mut history = vec![
        Message::system("You answer with the weather. Use the tool when asked."),
        Message::user("What is the weather in Lisbon?"),
    ];

    loop {
        let mut request = ModelRequest::new(model.reference().clone(), history.clone());
        request.tools = registry.specs();

        let mut stream = model.stream(request).await?;
        let mut accumulator = StreamAccumulator::new(StreamLimits::default());
        while let Some(event) = stream.next().await {
            let event = event?;
            if let StreamEvent::TextDelta { text, .. } = &event {
                print!("{text}");
            }
            accumulator.apply(event)?;
        }
        let finished = accumulator.finish()?;
        println!();

        // Every tool call the model made, in order.
        let calls: Vec<_> = finished
            .parts
            .iter()
            .filter_map(|part| match part {
                Part::ToolCall(call) => Some(call),
                _ => None,
            })
            .collect();

        if calls.is_empty() {
            // The model stopped asking: its answer is the last message.
            history.push(Message::new(Role::Assistant, finished.parts));
            break;
        }

        // The model's message (with its calls) goes back into history, then
        // one result per call — that is the shape the next request expects.
        history.push(Message::new(Role::Assistant, finished.parts.clone()));
        for call in &calls {
            let output =
                weather_tool::execute(&registry, &call.name, call.call_id, &call.arguments)
                    .await?;
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
    }

    for request in model.requests() {
        println!("--- request: {} message(s)", request.messages.len());
    }
    Ok(())
}
