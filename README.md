# RustyChain

A high-performance, type-safe Rust library for building LLM-powered applications,
chains, and agents.

RustyChain separates the **neutral model** (domain types, engine, streaming
pipeline, contracts) from the **wires** that speak to providers. The core carries
abstractions only: no vendor names, no model catalogs, no persistence backends.
Hosts supply capabilities, endpoints, credentials and storage; adapters translate
between the neutral model and the vendor wire in both directions.

## Crates

| Crate | Role |
| --- | --- |
| `rustychain-core` | Domain model, engine, streaming, policies, layers, capabilities and every contract (ports). |
| `rustychain-openai` | The `OpenAI` wire family: Responses (primary) and Chat Completions, plus the native `openai` preset. |
| `rustychain-anthropic` | The Anthropic Messages wire and the native `anthropic` preset. |
| `rustychain-gemini` | The Google Gemini `generateContent` wire and the native `google` preset. |
| `rustychain-http` | Shared HTTP transport, `VendorProfile`/auth schemes and `HttpMedia`. |
| `rustychain-tools` | The default host tool suite, including the `command` executor. |
| `rustychain-media` | Media preprocessing helpers. |
| `rustychain-mcp` | Model Context Protocol integration. |
| `rustychain-testing` | In-memory store, scripted model and conformance suites. |

## Installation

```toml
[dependencies]
rustychain-core = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
rustychain-openai = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
rustychain-testing = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
```

## Start

The engine is a pure state machine over a session. You give it a `Store`, a
`BlobStore`, a `Model` and a `ToolRegistry`; everything else is host data.

```rust
use std::sync::Arc;

use rustychain_core::{Capabilities, EngineBuilder, Message, Modality, ToolRegistry};
use rustychain_openai::presets::openai;
use rustychain_testing::{InMemoryBlobStore, InMemoryStore};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = openai("gpt-5", std::env::var("OPENAI_API_KEY")?, capabilities)?;

    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?;

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

`rustychain-testing`'s in-memory store is a reference implementation: production
hosts implement the `Store` and `BlobStore` ports against their own database and
object store.

## Turn control

`send` and `resume` use the engine defaults. To configure a single turn —
sampling, reasoning, and exactly which tools the model may call — open the turn
builder. Overrides are layered **per field**: engine defaults ← session defaults
← this turn.

```rust
use rustychain_core::{Message, ReasoningEffort, ReasoningRequest, ToolName};

async fn turn(engine: &rustychain_core::Engine, session: rustychain_core::SessionId)
    -> Result<(), Box<dyn std::error::Error>> {
    let outcome = engine
        .turn(session)
        .input(Message::user("Summarise this."))
        .temperature(0.2)
        .top_k(40)
        .reasoning(ReasoningRequest::Effort(ReasoningEffort::High))
        .tools([ToolName::new("echo")?])
        .start()
        .finish()
        .await?;
    let _ = outcome;
    Ok(())
}
```

Only the named tools are exposed to the model for that request — the base case
is every registered tool. The effective configuration is reported on every
request through `EngineEvent::RequestReconstructed`, so each send is auditable.

## History as data

A session exports as a self-contained, serializable `Conversation`: the ordered
message array plus its identity. It round-trips losslessly and byte-stably, and a
re-import reproduces the exact wire prefix — including the provider cache key —
so handing a chat to another process does not cost a cache miss.

```rust
use rustychain_core::Conversation;

async fn handover(engine: &rustychain_core::Engine, session: rustychain_core::SessionId)
    -> Result<(), Box<dyn std::error::Error>> {
    let conversation = engine.export(session).await?;
    let json = conversation.to_json()?; // store it, ship it
    let restored = Conversation::from_json(&json)?;
    let imported = engine.import(restored).await?;
    let _ = imported;
    Ok(())
}
```

## Vendors

Every adapter is parameterized by three host values: a `ModelRef`, a
`VendorProfile` (endpoint + credentials) and a `Capabilities` descriptor. The
native vendors ship as presets (`openai`, `anthropic`, `gemini`). Compatible
vendors need no preset: they are a `VendorProfile` with a different base URL and
the generic adapter constructor.

```rust
use rustychain_core::{Capabilities, ModelRef, Modality, ProviderId, WireFamily};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::OpenAiChat;

fn openrouter() -> Result<OpenAiChat, Box<dyn std::error::Error>> {
    let profile = VendorProfile::new(
        ProviderId::new("openrouter")?,
        WireFamily::new("openai.chat")?,
        "https://openrouter.ai/api/v1",
    )?
    .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let reference = ModelRef::new(
        profile.provider.clone(),
        "anthropic/claude-sonnet-4".parse()?,
    );
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    OpenAiChat::new(reference, profile, capabilities)
}
```

Capabilities are typed and host-supplied. There is no built-in model table. The
fluent builder is the primary API; `capabilities_from_json` exists only so a host
can bridge its own configuration.

## Tools

Tools are declared with an attribute and executed by the host.

```rust
use rustychain_core::{FnExecutor, Result, ToolRegistry};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoResult {
    echoed: String,
}

#[rustychain_core::tool(name = "echo", description = "Echo the provided text")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }
}

fn build_registry() -> Result<ToolRegistry> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo)?;
    Ok(tools)
}
```

Rich, stateful tools register through `ToolRegistry::register_tool` and can stream
progress, observe cancellation and render custom context.

## Documentation

- [`docs/SRS.md`](docs/SRS.md) is the single source of truth for behaviour.
- [`CONTRIBUTING.md`](CONTRIBUTING.md) covers the build, lint and controller
  policies.

## License

MIT.
