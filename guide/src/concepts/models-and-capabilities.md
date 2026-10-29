# Models and capabilities

## Capabilities: declared, not looked up

The library ships no model catalogue. The host declares what a model can do, and
the engine holds requests to that declaration.

```rust,ignore
use norte_core::{CacheMode, Capabilities, Modality, StreamingGranularity, ToolKind};

let capabilities = Capabilities::builder()
    .input(Modality::Text)
    .input(Modality::Image)
    .input_limits(Modality::Image, limits)
    .output(Modality::Text)
    .reasoning(reasoning)
    .tool_kind(ToolKind::Function)
    .feature(feature)
    .streaming(StreamingGranularity::Delta)
    .structured_output(mode)
    .cache(CacheMode::Automatic)
    .max_context(200_000)
    .max_output(8_192)
    .build();
```

| Enum | Variants |
| --- | --- |
| `Modality` | `Text`, `Image`, `Audio`, `Video`, `Document` |
| `ToolKind` | `Function` (host executes), `ProviderExecuted`, `ProviderSchema` |
| `StreamingGranularity` | `Delta`, `Part`, `Disabled` |
| `CacheMode` | `None`, `Automatic`, `Explicit` |
| `ReasoningRequest` | the reasoning ask: automatic, an effort, a budget, or off |
| `Feature` | `Citations`, `ParallelToolCalls`, `ForcedToolChoice`, `Batch`, `Logprobs` |

A request that asks for something outside the declared capabilities is a typed
error before the wire is touched, not a vendor error afterwards.

`Feature::Logprobs` gates token probabilities: a turn asks for a count and gets a
typed error when the model does not declare the feature, so the host declares it
once per model instead of discovering the gap on the wire.

## ModelRef: who answers

```rust,ignore
use norte_core::{ModelId, ModelRef, ProviderId};

let reference = ModelRef::new(
    ProviderId::new("openrouter")?,
    ModelId::new("deepseek/deepseek-v4.1-flash-1017")?,
);
assert_eq!(reference.to_string(), "openrouter/deepseek/deepseek-v4.1-flash-1017");
```

- `ProviderId` is a name used to route and to label history. It allows
  `[a-z0-9_-]`, at most 128 bytes.
- `ModelId` is the id the wire expects, `[A-Za-z0-9-_.:/@]`.
- `ModelRef` is the pair, `Display`ed as `provider/model`, and parses back from
  that form splitting on the first `/`.

`ModelRef` appears in requests and in the history — but it is not a lookup key. Nothing in the engine resolves it to a
price, a context window or an endpoint. That is the wire adapter's business, and
the host's declaration.

## The model port

`Arc<dyn Model>` is the only way the engine talks to a provider. A `Model` takes
a `ModelRequest` and returns a `ModelStream` of `StreamEvent`s; the engine's
accumulator turns that into `Part`s. Everything above this line — sessions,
interceptors, durability, cost — is provider-independent.

Two consequences worth knowing:

- **Testing needs no network.** `norte-testing`'s `ScriptedModel` is a
  `Model`; so is `ProbeModel`, which wraps another model and records the requests
  that went through it. See [Test without a network](../guides/test-without-a-network.md).
- **A missing field is not a failure.** When a wire reports a block the neutral
  model has no field for, it is kept as a `ProviderToolCall`,
  `ProviderToolResult` or `Extension` part and replayed unchanged. Nothing is
  silently dropped.

## Request reconstruction

Before a request goes out, the engine assembles it from the committed history and
announces `RequestReconstructed { digest, cache_boundaries, params, reasoning,
tools }`. The `digest` is a content hash of the request, which makes two turns
comparable; `cache_boundaries` are the positions the provider may cache up to.

This event is the single best place to log an agent's iterations: it fires once
per model request, including every round of a tool loop, which a turn's outcome
does not show. `benchmarks/agent-cost` prints one header per occurrence.
