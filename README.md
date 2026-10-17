<p align="center">
  <img src="assets/banner.jpg" alt="RustyChain" width="100%">
</p>

# RustyChain

<p align="center">
  <a href="https://github.com/JeanVydes/rustychain/actions/workflows/rust.yml"><img src="https://github.com/JeanVydes/rustychain/actions/workflows/rust.yml/badge.svg?branch=dev" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/MSRV-1.85-blue.svg" alt="MSRV: 1.85">
  <img src="https://img.shields.io/badge/edition-2024-blue.svg" alt="Edition: 2024">
</p>

Streaming-first, type-safe Rust for LLM apps, chains and agents. A neutral core
with no vendor names, plus adapters for OpenAI, Anthropic and Gemini.

- **Neutral core** — vendor types never leave an adapter.
- **Stream-first** — text, reasoning, tool arguments, media and safety ratings
  arrive as deltas; blocking is `collect()`.
- **Durable sessions** — a turn is a commit on an append-only node graph.
- **Lossless export** — `Conversation` round-trips byte-for-byte, cache prefix
  included.
- **Policy-gated tools** — allow, deny, require approval, rewrite or rate-limit.
- **Ports, not backends** — implement `Store` and `BlobStore`; in-memory versions
  ship for tests.

## Install

Not published yet:

```toml
[dependencies]
rustychain-core = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
rustychain-openai = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
rustychain-testing = { git = "https://github.com/JeanVydes/rustychain", branch = "dev" }
```

## Quickstart

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

The in-memory store is a test reference; production hosts implement `Store` and
`BlobStore` against their own database and object storage.

## Streaming

A turn yields `EngineEvent`s as they arrive; `finish()` returns the outcome.
Dropping the stream cancels the turn. Tools run automatically, up to
`EngineConfig::max_iterations`.

```rust
use futures_util::StreamExt;
use rustychain_core::{Engine, EngineEvent, Message, SessionId, StreamDelta};

async fn stream_turn(
    engine: &Engine,
    session: SessionId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = engine.send(session, Message::user("Hello!"));

    while let Some(event) = stream.next().await {
        match event? {
            EngineEvent::StreamDelta {
                delta: StreamDelta::Text { text, .. },
                ..
            } => print!("{text}"),
            EngineEvent::StreamDelta {
                delta: StreamDelta::Reasoning { text, .. },
                ..
            } => eprint!("{text}"),
            EngineEvent::ToolCallStarted { tool, .. } => eprintln!("\n→ {tool}"),
            _ => {}
        }
    }

    let outcome = stream.finish().await?;
    let _ = outcome;
    Ok(())
}
```

## Tools

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

The argument schema is generated from `EchoArgs`. Rich tools use
`ToolRegistry::register_tool` and can stream progress or observe cancellation.

## Approvals

`ToolApprovalPolicy` gates a tool: the session parks at `Phase::AwaitingApproval`
until the host grants or denies the call. Grants are bound to the arguments, so
they cannot be replayed for different input.

```rust
use std::sync::Arc;

use rustychain_core::{GrantScope, Message, Phase, PolicySet, ToolApprovalPolicy, ToolName};

fn shell_approvals() -> Result<PolicySet, Box<dyn std::error::Error>> {
    Ok(PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "shell-approval",
        vec![ToolName::new("command")?],
        "the agent wants to run a shell command",
    ))]))
}

async fn run_guarded(
    engine: &rustychain_core::Engine,
    session: rustychain_core::SessionId,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = engine
        .send(session, Message::user("Clean the build directory."))
        .finish()
        .await?;

    if outcome.session.phase == Phase::AwaitingApproval {
        let pending = engine.pending_approvals(session).await?;
        for record in &pending {
            println!("approve `{}`?", record.tool);
        }
        let record = &pending[0];
        // Deny instead with `engine.deny(record.id, "operator", reason)`.
        engine
            .approve(record.id, GrantScope::Call(record.call_id), "operator")
            .await?;
        engine.resume(session).finish().await?;
    }
    Ok(())
}
```

## Turn control

`send` and `resume` use the engine defaults. Configure one turn — sampling,
reasoning, exposed tools — with the turn builder. Overrides layer per field:
engine ← session ← turn.

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

## Other vendors

An adapter is a `ModelRef` + a `VendorProfile` (endpoint, auth) + `Capabilities`.
OpenAI-compatible vendors need no preset:

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

Presets: `rustychain_openai::presets::openai`,
`rustychain_anthropic::presets::anthropic`, `rustychain_gemini::presets::gemini`.

## Export & import

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

A re-import reproduces the exact wire prefix, so it costs no cache miss.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
```

[`docs/SRS.md`](docs/SRS.md) is the source of truth for behaviour;
[`CONTRIBUTING.md`](CONTRIBUTING.md) covers the build and lint policies. Wire
parsers are fuzzed with `cargo-fuzz` (`fuzz/`); live tests are `#[ignore]`d.

## License

MIT.
