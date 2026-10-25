<p align="center">
  <img src="https://raw.githubusercontent.com/JeanVydes/Norte/dev/assets/banner.jpg" alt="Norte" width="100%">
</p>

# Norte

<p align="center">
  <a href="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml"><img src="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml/badge.svg?branch=dev" alt="CI"></a>
  <a href="https://github.com/JeanVydes/Norte/blob/dev/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/MSRV-1.85-blue.svg" alt="MSRV: 1.85">
  <img src="https://img.shields.io/badge/edition-2024-blue.svg" alt="Edition: 2024">
</p>

**Provider-agnostic infrastructure for AI agents in Rust.** One core, three
first-class provider SDKs, a durable conversation model, and a reference agent
loop you may take or leave.

Nothing in your application names a vendor, and no model catalogue is baked in:
you declare what your models can do, and the requests follow, every call under a
policy you write, every turn in a history you can audit.

## The core does not know what an agent is

Norte is split so that the agent part is *optional*, not assumed.

- **`norte-core`** owns what a provider sees. The port is one method:

  ```rust
  async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
  ```

  and a `ModelRequest` carries `messages: Vec<Message>` — the conversation
  already flattened to a list. Beside it: `Part`, `Role`, `Usage`,
  `StreamEvent`, the `Model`/`BlobStore`/`MediaAdapter` ports, `Layer` and
  `Observer`.
- **`norte-harness`** owns everything that exists because something *walks* that
  list forward: the branch graph (`Session`, `Commit`, `Node`, `Conversation`),
  the store it commits through, the tool loop, policies, approvals, and the
  `EngineEvent`s it reports. `Harness` is one way to drive the port, and *a*
  harness rather than *the* harness.

  The rule in one line: **the core knows a list of messages; the harness knows a
  graph.**

A host that wants a different turn policy — its own scheduling, a graph instead
of a session, a batch job with no loop at all — depends on `norte-core` alone
and never sees the harness. That is the point of the split: the contracts are the
product, and the loop is one consumer of them.

## Why Norte

- **Scalable.** Sessions live in a store you choose, not in process memory: add
  workers, restart them mid-turn and share a session — with optimistic
  concurrency instead of a coordinator.
- **Extensible at every seam.** Providers, stores, tools, policies, layers and
  observers are traits. Bring your own model, database, object storage or
  toolchain; the engine holds no vendor and no persistence of its own.
- **Governed.** Every tool call passes a policy before it runs — allow, deny,
  rewrite the arguments or require a human — and an approval is bound to the
  exact arguments it was granted for.
- **Auditable.** Every turn is one atomic commit carrying the model, the token
  counters and the cost the provider reported. A session exports and re-imports
  byte for byte, for a review, a replay or a hand-off.
- **Secure.** `unsafe` is forbidden, every wire parser is fuzzed, secrets are
  redacted from tool output, and failures are typed errors rather than panics.
- **Private by default.** No telemetry and no call home: the only network traffic
  is to the endpoint you configure. Bring your own inference — any
  OpenAI-compatible server — and your own database and object storage.
- **Observable.** Every request, tool call, token count and cache boundary is an
  event, streamed as it happens.
- **Cost you can attribute.** Usage and cost come from the provider's own
  payloads, per request and per turn: chargeback-ready, with no rate table to
  maintain.

## Providers and coverage

| Provider | Crate | APIs | Coverage |
| --- | --- | --- | --- |
| OpenAI | `norte-openai` | Responses, Chat Completions | `███████████████████░` 95 % |
| Anthropic | `norte-anthropic` | Messages | `███████████████████░` 95 % |
| Google Gemini | `norte-gemini` | generateContent, cached content | `███████████████████░` 95 % |

Coverage is the share of each vendor's documented API the adapter implements.
Two directions are out of scope by decision and will not be implemented: the
Realtime API and unary `stream: false` delivery (deprecated as a direction —
`collect()` already covers blocking callers). A modality an API cannot express
is a typed error, never a silent drop, and a documented block with no neutral
form is kept verbatim and replayed unchanged.

Anything OpenAI-compatible works through the OpenAI adapter with your own base
`URL`: OpenRouter, Groq, Together, vLLM, an internal gateway or a self-hosted
model server. The endpoint is configuration, so an on-prem deployment is a `URL`
and a key.

## Install

```toml
[dependencies]
norte = { version = "0.0.1", features = ["harness", "tools"] }
```

The core, the providers and their contracts come by default. `harness` adds the
agent loop, `tools` the default tool suite; a host that writes its own loop
leaves `harness` out and never compiles it.

## Quickstart

The core is a model port and the value types a conversation is made of. No
harness, no session, no framework — one request, every event:

```rust
use std::error::Error;

use futures_util::StreamExt;
use norte_core::{Capabilities, Message, Modality, Model, ModelRef, ModelRequest, StreamingGranularity, WireFamily};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let model = OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            std::env::var("COOKBOOK_MODEL")?.as_str().try_into()?,
        ),
        VendorProfile::new(
            "openrouter".try_into()?,
            WireFamily::new(WIRE_CHAT)?,
            &std::env::var("COOKBOOK_ENDPOINT")?,
        )?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?)),
        Capabilities::builder()
            .input(Modality::Text)
            .output(Modality::Text)
            .streaming(StreamingGranularity::Delta)
            .build(),
    )?;

    let mut stream = model
        .stream(ModelRequest::new(
            model.reference().clone(),
            vec![
                Message::system("Answer in one sentence."),
                Message::user("What is a prompt cache?"),
            ],
        ))
        .await?;

    while let Some(event) = stream.next().await {
        println!("{:?}", event?);
    }

    Ok(())
}
```

A model id and a profile are all the configuration a call needs: the adapter is
chosen by name, the endpoint is a `URL`, and the key comes from the environment.

To drive a *session* instead — history, tools, approvals, one atomic commit per
turn — reach for the harness, which is one client of this port among others:

```rust
use std::sync::Arc;

use norte::prelude::*;

let harness = HarnessBuilder::new()
    .store(Arc::new(InMemoryStore::new()))
    .blobs(Arc::new(InMemoryBlobStore::new()))
    .model(Arc::new(model))
    .tools(ToolRegistry::new())
    .build()?;

let session = harness.create().await?;
let outcome = harness
    .send(session.id, Message::user("Hello!"))
    .finish()
    .await?;
println!("{}", outcome.message.as_ref().map(Message::text).unwrap_or_default());
```

`Store` and `BlobStore` are ports. The in-memory pair above implements the whole
contract — one atomic commit per turn, optimistic concurrency, the approval
ledger, content-addressed bytes — and is the right default for a CLI, a desktop
app or a single-process worker. When a session has to outlive the process,
implement the two traits over your own database and object storage.

## Examples

Runnable, end-to-end programs live in
[`examples/`](https://github.com/JeanVydes/Norte/tree/dev/examples):

- [`cookbook`](https://github.com/JeanVydes/Norte/tree/dev/examples/cookbook) —
  one small, self-contained recipe per topic: events, streaming, approval,
  policies, declaring tools, observers, idempotent tools.

Measurements live in
[`benchmarks/`](https://github.com/JeanVydes/Norte/tree/dev/benchmarks): each one
is a folder with the program, how to run it, and the reports it writes.

## Documentation

- **The guide** — [the hosted guide](https://jeanvydes.github.io/Norte/)
  explains what the library offers, the concepts behind it, and how to do
  specific things. It is written in Markdown and built with
  [mdBook](https://rust-lang.github.io/mdBook/); run `mdbook serve guide` to read
  it locally, and CI publishes it to GitHub Pages from `dev`.
- **Requirements** — [the requirements ledger][ledger] is the normative contract:
  every requirement with its id, the code that enforces it, and the test or CI
  job that checks it. The ids in source comments and workflow names resolve
  there; the source lives in `guide/src/reference/requirements.md`.
- **Rustdoc** — every crate documents its public surface; the reference is
  generated, not written twice:

  ```sh
  cargo doc --workspace --no-deps --open
  ```

- **Examples** — [the cookbook][cookbook] is one small recipe per topic, each
  runnable on its own.
- **Contributing** — [`CONTRIBUTING.md`][contributing] covers the layout, the
  build and the lint policies.
- **Changelog** — [`CHANGELOG.md`][changelog] records what each version changed.

[ledger]: https://jeanvydes.github.io/Norte/reference/requirements.html
[cookbook]: https://github.com/JeanVydes/Norte/tree/dev/examples/cookbook
[contributing]: https://github.com/JeanVydes/Norte/blob/dev/CONTRIBUTING.md
[changelog]: https://github.com/JeanVydes/Norte/blob/dev/CHANGELOG.md

## Development

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --document-private-items
```

Wire parsers are fuzzed with `cargo-fuzz` (`fuzz/`); live provider tests are
`#[ignore]`d.

## License

MIT.
