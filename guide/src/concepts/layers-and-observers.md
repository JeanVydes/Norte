# Layers and observers

Two extension points sit between a turn and the wire, and one sits beside it.

## Layers: in front of the provider

A `Layer` wraps the model call. It receives the assembled `ModelRequest` and a
continuation, and it may pass the request on, change it, or answer it itself.

```rust,ignore
use norte_core::{Continuation, Layer, LayerContext, ModelRequest, ModelStream, Result};

struct PinRoute;

#[async_trait::async_trait]
impl Layer for PinRoute {
    fn name(&self) -> &'static str {
        "pin-route"
    }

    async fn wrap(
        &self,
        context: &LayerContext<'_>,
        mut request: ModelRequest,
        next: &dyn Continuation,
    ) -> Result<ModelStream> {
        request.extensions.insert(
            "openai.chat:provider".to_owned(),
            serde_json::json!({ "only": ["DeepInfra"], "allow_fallbacks": false }),
        );
        next.call(context, request).await
    }
}

let engine = EngineBuilder::new()
    // ..
    .layers(LayerStack::new(vec![std::sync::Arc::new(PinRoute)]))
    .build()?;
```

`LayerContext` carries the session, the turn, and an `EventSink`. A layer that
answers without the provider calls `context.short_circuit(name, reason)` and
emits its own synthetic stream (`synthetic_stream(events)`), which is announced as
`LayerShortCircuited { layer, reason }` with a `ShortCircuitReason`: `CacheHit`,
`Rejected { detail }`, `Local { detail }` or `Custom { name, detail }`.

Layers are the right place for: routing hints, a local answer cache, request
redaction, an offline mode, or replaying a recorded response.

## Observers: beside the turn

An `Observer` sees every `EngineEvent`. It cannot change the turn; it watches it.

```rust,ignore
use norte_core::{EngineEvent, Observer};

struct Metrics;

impl Observer for Metrics {
    fn name(&self) -> &'static str {
        "metrics"
    }

    fn observe(&self, event: &EngineEvent) {
        if let EngineEvent::RequestReconstructed { turn, tools, .. } = event {
            // one call per model request: count it
            let _ = (turn, tools);
        }
    }
}

let engine = EngineBuilder::new()
    // ..
    .observers(vec![std::sync::Arc::new(Metrics)])
    .build()?;
```

A `StreamTransform` sits closer to the tokens: `transform(delta) -> Vec<StreamDelta>`
can rewrite or drop a delta before your loop sees it (redaction of a secret that
leaked into the stream, for instance). Register with `.transform(..)` or
`.transforms(vec![..])`.

`ObserverSet::new(observers, buffer)` is the underlying type; it buffers events so
a slow observer cannot stall a turn, and `dropped()` counts what did not fit —
`EngineConfig::limits` bounds the same budget. Observer and transform names are
recorded in the session metadata, so a stored session says how it was watched.

## Cache boundaries, seen from outside

Two events are about the provider cache rather than about content:

- `RequestReconstructed { digest, cache_boundaries, .. }` — the request is
  assembled; `cache_boundaries` are the positions the provider may cache up to,
  and `digest` is a content hash of the whole request.
- `PrefixInvalidated { reason }` — the prefix cannot be reused, with the reason.

Between them, a host can see exactly when an agent's transcript stopped being
cacheable — usually the interesting question when a long session's bill jumps.
There is no cache *type* to configure: the boundaries come from `CacheClass` on
messages, and the provider decides what it honours.

## Knobs on the engine

`EngineConfig` holds the turn-level policy that is not a layer:

| Field | Default | What it does |
| --- | --- | --- |
| `reasoning` | automatic | The default reasoning ask. |
| `params` | `GenerationParams::default()` | Default generation parameters. |
| `limits` | `StreamLimits::default()` | Bounds on buffered events and stream size. |
| `inline_threshold` | 64 KiB | Above this, media goes to the `BlobStore`. |
| `approval_ttl` | 10 minutes | How long a pending approval lives. |
| `max_iterations` | 16 | Requests a single turn may spend before it errors. |
| `auto_execute_tools` | `true` | Run allowed tool calls inside the turn. |
| `early_tool_execution` | `false` | Start a tool as soon as its call is complete, before the message ends. |

`max_iterations` is a chat turn's 16 by default. A coding agent reads, edits and
reads back; a coding agent raises it to 32. Note what happens when it is exceeded:
the turn errors, **after** every request it made has been committed and billed.
That is why a cost meter must keep counting past the error.
