# Streaming

A turn is a stream, always. `finish()` is the convenience that drains it.

```rust,ignore
use futures_util::StreamExt;

let mut stream = engine.send(session.id, Message::user("Hello!")).start();
while let Some(event) = stream.next().await {
    match event? {
        EngineEvent::StreamDelta { delta, .. } => print!("{delta:?}"),
        _ => {}
    }
}
let outcome = stream.finish().await?;
```

`EngineStream` is a `Stream<Item = Result<EngineEvent>>` with `cancel()`. If it is
dropped without `finish()`, the turn is cancelled — `finish()` takes ownership on
purpose, because it is the call that waits for the commit.

## StreamDelta — the tokens

`StreamDelta` is what the provider sent, translated into one vocabulary:

| Variant | Meaning |
| --- | --- |
| `Text { index, text }` | Answer text. |
| `Reasoning { index, text }` | Reasoning text the provider streamed. |
| `Refusal { index, text }` | A refusal. |
| `ToolArguments { index, call_id, arguments }` | A fragment of a tool call's JSON arguments. |
| `Media { index, bytes, last }` | Media bytes arrived. |
| `Continuity { index, len }` | An opaque continuity token arrived. |
| `SafetyRatings { ratings }` | The provider's ratings for this chunk. |

`index` identifies the part the delta belongs to, so a message with text *and* a
tool call streams both without losing track.

## EngineEvent — the turn

`EngineEvent` is the turn's lifecycle, and it is what a UI, a logger or a metric
subscribes to:

| Event | When |
| --- | --- |
| `SessionCreated { session, phase }` | `engine.create()`. |
| `PhaseChanged { session, from, to }` | Any phase transition. |
| `StreamDelta { session, turn, delta }` | Every chunk, as above. |
| `PartCompleted { session, turn, part }` | A part the wire closed explicitly. |
| `ToolCallStarted { session, turn, call_id, tool }` | A tool call is about to run. |
| `ToolCallFinished { session, turn, call_id, tool, is_error, duration_ms }` | It ran. |
| `ApprovalRequested { session, approval, call_id, tool }` | A policy asked for a human. |
| `ApprovalGranted { session, approval }` / `ApprovalRevoked { session, approval, grant }` | A decision was recorded. |
| `HistoryAppended { session, messages }` | Messages were appended. |
| `CommitSucceeded { session, version }` | The turn committed. |
| `CommitConflict { session, expected, actual }` | Someone else committed first. |
| `LayerShortCircuited { session, layer, reason }` | A layer answered without the provider. |
| `RequestReconstructed { session, turn, digest, cache_boundaries, params, reasoning, tools }` | The request is assembled; this is the iteration boundary. |
| `PrefixInvalidated { session, reason }` | The provider cache prefix cannot be reused. |
| `Error { session, message }` | Something failed inside the turn. |
| `Extension { namespace, data }` | A namespaced event from a layer or observer. |

`EngineEvent::session()` returns the session a routed event belongs to.

### What is *not* an event

Two things people expect to find in the stream are not in it:

- **`PartCompleted` is not "every part".** It fires for a part the *wire* closed
  explicitly — a tool call whose block carries its own completion event, a
  provider tool call or result, a whole `Part`, the closing media chunk. Text and
  reasoning parts, and a tool call whose wire signals no completion, are held in
  the accumulator and handed back when the stream ends.
- **A tool result is not a stream event.** It is produced after the assistant
  message commits, and reaches the host through the store. To render it, read
  `engine.history(session)` — which is what the cookbook's `agent_cost` does.

If you write a renderer, the safe pattern is: stream `StreamDelta` for live text,
and read history when a `RequestReconstructed` tells you the next request is
being built. Everything settled is there, in order.

## Observers and transforms

An `Observer` sees every `EngineEvent`; a `StreamTransform` rewrites or drops
deltas before they reach your loop. Both are registered on the builder. See
[Layers and observers](concepts/layers-and-observers.md).

## Errors inside a stream

An event that is `Err` is a failure of that turn; the stream may end there. Note
that a turn which runs out of its iteration budget ends with an error *after*
every request it made has been committed and billed — which is exactly the case a
cost meter has to keep counting. The cookbook's `agent_cost` drains instead of
`?`-ing for that reason.
