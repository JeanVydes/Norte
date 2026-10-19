# Getting started

## Install

The crates are not published yet; depend on the repository:

```toml
[dependencies]
rustychain-core = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
rustychain-openai = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

`rustychain-core` is the engine and the contracts; a wire crate is a provider.

## A turn, end to end

```rust,ignore
use std::sync::Arc;

use rustychain_core::{
    Capabilities, EngineBuilder, InMemoryBlobStore, InMemoryStore, Message, Modality, ToolRegistry,
};
use rustychain_openai::presets::openai;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. What the model can do — declared, not looked up.
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = openai("gpt-5", std::env::var("OPENAI_API_KEY")?, capabilities)?;

    // 2. The engine: one store, one blob store, one model, a tool registry.
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?;

    // 3. A session, and one turn in it.
    let session = engine.create().await?;
    let outcome = engine
        .send(session.id, Message::user("Hello!"))
        .finish()
        .await?;

    println!(
        "{}",
        outcome.message.as_ref().map(Message::text).unwrap_or_default()
    );
    Ok(())
}
```

### What each line did

- `Capabilities::builder()` — you declare the modalities, tool kind, streaming
  granularity and cache mode the model supports. The engine checks requests
  against this before it builds one, so a request the model cannot serve is an
  error here rather than a surprise from the wire. See
  [Models and capabilities](concepts/models-and-capabilities.md).
- `openai(model, key, capabilities)` — a native preset: the `Responses` wire, the
  right auth header, the right base `URL`. The generic form is a
  `VendorProfile` plus `OpenAiChat::new(..)`, for any `OpenAI`-compatible
  endpoint. See [Providers](concepts/providers.md).
- `EngineBuilder` — `store`, `blobs` and `model` are required; everything else has
  a default. `build()` fails with `Error::InvalidInput` if one is missing.
- `engine.create()` — creates a session, commits it, and emits `SessionCreated`.
  A session is durable from that moment.
- `engine.send(id, message)` — opens a turn with a user message and returns an
  `EngineStream`. `finish()` drains it and returns a `TurnOutcome` with the
  session and the assistant message that was committed.
- `outcome.message` is `Option<Message>`: a turn that ended in a tool call may
  commit no assistant message of its own.

## Streaming instead of waiting

`finish()` is the blocking shape. The same turn as a stream:

```rust,ignore
use futures_util::StreamExt;

let mut stream = engine.send(session.id, Message::user("Hello!")).start();
while let Some(event) = stream.next().await {
    let event = event?;
    // EngineEvent::StreamDelta { delta, .. } carries the tokens.
    let _ = event;
}
let outcome = stream.finish().await?;
```

`start()` gives you the events as they arrive; `finish()` still has to be called
(it consumes the stream and commits the turn). Dropping the stream without
`finish()` cancels the turn. See [Streaming](concepts/streaming.md).

## Pointing it somewhere else

Any `OpenAI`-compatible endpoint is a profile, not a preset:

```rust,ignore
use rustychain_core::{ModelId, ModelRef, ProviderId, WireFamily};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};

let provider = ProviderId::new("openrouter")?;
let profile =
    VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, "https://openrouter.ai/api/v1")?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
let model = OpenAiChat::new(
    ModelRef::new(provider, ModelId::new("deepseek/deepseek-v4-flash-0731")?),
    profile,
    capabilities,
)?;
```

That is the whole difference between vendors at the call site. Three native
presets (`openai`, `anthropic`, `gemini`) and this generic form cover everything
in [Providers](concepts/providers.md).

## Runnable examples

The repository ships a cookbook of small, self-contained recipes — one file per
topic, each wiring its own engine:

```sh
OPENROUTER_API_KEY=sk-or-... \
COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
COOKBOOK_MODEL=deepseek/deepseek-v4-flash-0731 \
cargo run -p rustychain-cookbook --example events
```

`events`, `streaming`, `approval`, `policy`, `policy_context`, `tools`, `agent`,
`executors`, `observers`, `cache`, `export_import`, `branching`. Each file is
meant to be read and copied on its own. `rustychain-cookbook` is not a library:
nothing is shared between recipes on purpose.
