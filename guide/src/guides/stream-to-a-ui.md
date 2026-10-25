# Stream to a UI

A turn is a stream of `EngineEvent`s, and a UI wants three things from it: text as
it lands, honest indicators for everything that is not text, and a correct final
state.

## The loop

```rust,ignore
use futures_util::StreamExt;
use norte_core::{EngineEvent, StreamDelta};

let mut stream = engine.send(session.id, Message::user(prompt)).start();

while let Some(event) = stream.next().await {
    match event? {
        EngineEvent::StreamDelta { delta, .. } => match delta {
            StreamDelta::Text { text, .. } => ui.append_answer(&text),
            StreamDelta::Reasoning { text, .. } => ui.append_thinking(&text),
            StreamDelta::Refusal { text, .. } => ui.append_refusal(&text),
            StreamDelta::ToolArguments { call_id, .. } => ui.note_arguments_arriving(call_id),
            StreamDelta::Media { bytes, .. } => ui.note_media(bytes),
            StreamDelta::Continuity { .. } => {}
            StreamDelta::SafetyRatings { ratings } => ui.show_ratings(&ratings),
            _ => {}
        },
        EngineEvent::ToolCallStarted { tool, .. } => ui.tool_started(&tool),
        EngineEvent::ToolCallFinished { tool, is_error, duration_ms, .. } => {
            ui.tool_finished(&tool, is_error, duration_ms);
        }
        EngineEvent::ApprovalRequested { approval, tool, .. } => ui.ask_human(approval, &tool),
        EngineEvent::RequestReconstructed { digest, tools, cache_boundaries, .. } => {
            ui.new_iteration(digest, tools.len(), cache_boundaries.len());
            // Everything settled is in history now — this is the moment to read it.
            for message in engine.history(session.id).await? {
                for part in &message.parts {
                    ui.show_part(&part);   // tool calls and tool results land here
                }
            }
        }
        EngineEvent::CommitSucceeded { version, .. } => ui.mark_saved(version),
        EngineEvent::Error { message, .. } => ui.show_error(&message),
        _ => {}
    }
}

let outcome = stream.finish().await?;
ui.final_state(outcome.session.phase);
```

## Three things that surprise people

**A message can carry text and a tool call at once.** The text that arrives before
a call is narration, not an answer. Do not treat the first `StreamDelta::Text` as
final, and do not clear the narration when a call starts — show it and keep going.

**A tool result is not a stream event.** The stream tells you a call *started* and
*finished*; the bytes it returned are committed to the store instead. Read
`engine.history(session.id)` when `RequestReconstructed` fires — that is the exact
moment the next request is being built out of them, so it is when they are worth
showing. A UI that only wires the stream will render "the agent read the file" but
never the file's contents.

**`PartCompleted` is not every part.** It fires for the parts the wire closes
explicitly. Text and reasoning parts are handed back when the stream ends instead.
Do not build a UI that depends on receiving them as events; use `StreamDelta` for
live text and history for settled parts.

## Cancellation

```rust,ignore
stream.cancel();          // asks the turn to stop
let _ = stream.finish().await;   // waits for the commit that closes it
```

Dropping an `HarnessStream` cancels it. `finish()` is the only call that waits for
the commit, so call it even when you cancelled — a cancelled turn still has to
leave the session in a consistent version.

The same `CancellationToken` reaches the tool executors through `ToolContext`, so
a long command stops with the turn.

## Cost, on screen

If the UI shows a running bill, read it from history alongside the parts, never
from a rate table: `MessageOrigin::usage` has the counters, and the provider's
own price, when it reports one, is in `Usage::vendor["cost"]`. See
[Usage and cost](../concepts/usage-and-cost.md).

## Reference implementation

The `streaming` recipe in `examples/cookbook` is the minimal loop, and
`benchmarks/agent-cost` is the maximal one: it renders text, reasoning, each
iteration with its request digest and cache boundaries, every tool call with the
bytes it returned, and the provider's bill per request — all of it from the event
stream plus history.
