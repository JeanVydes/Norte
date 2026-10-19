<p align="center">
  <img src="https://raw.githubusercontent.com/JeanVydes/Norte/dev/assets/banner.jpg" alt="Norte" width="100%">
</p>

# Norte

<p align="center">
  <a href="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml"><img src="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml/badge.svg?branch=dev" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/MSRV-1.85-blue.svg" alt="MSRV: 1.85">
  <img src="https://img.shields.io/badge/edition-2024-blue.svg" alt="Edition: 2024">
</p>

**Provider-agnostic infrastructure for AI agents in Rust.** One core, three
first-class provider SDKs, and a streaming, durable engine.

Nothing in your application names a vendor, and no model catalogue is baked in:
you declare what your models can do, and the engine does the rest — every call
under a policy you write, every turn in a history you can audit.

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
- **Observable.** Every request, tool call, token count and cache boundary is an
  event, streamed as it happens.
- **Cost you can attribute.** Usage and cost come from the provider's own
  payloads, per request and per turn: chargeback-ready, with no rate table to
  maintain.

## Providers

| Provider | SDK | Wire surface |
| --- | --- | --- |
| OpenAI | `norte-openai` | Responses (primary) and Chat Completions, including the compatible long tail |
| Anthropic | `norte-anthropic` | Messages |
| Google Gemini | `norte-gemini` | generateContent, with explicit cached content |

Each adapter covers roughly 95 % of that wire's documented surface. What is
missing is small and named: the Realtime API (out of scope), `logprobs`, and
unary `stream: false` delivery (`collect()` already covers blocking callers). A
modality a wire cannot express is a typed error, never a silent drop, and a
documented block with no typed neutral form is kept verbatim and replayed
unchanged. OpenRouter, Groq, Together, vLLM and any other OpenAI-compatible
endpoint work through the OpenAI SDK with a different base URL.

## Install

```toml
[dependencies]
norte = "0.0.1"
```

## Quickstart

```rust
use std::sync::Arc;

use norte::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = norte::presets::openai("gpt-5", std::env::var("OPENAI_API_KEY")?, capabilities)?;

    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?;

    let session = engine.create().await?;
    let outcome = engine.send(session.id, Message::user("Hello!")).finish().await?;
    println!("{}", outcome.message.as_ref().map(Message::text).unwrap_or_default());
    Ok(())
}
```

`Store` and `BlobStore` are ports. The in-memory pair above is ready to use and
implements the whole contract — one atomic commit per turn, optimistic
concurrency, the tool-approval ledger, content-addressed bytes. It is the right
default for a CLI, a desktop app, a prototype or a single-process worker; when a
session has to outlive the process, implement the two traits over your own
database and object storage. Nothing else in the engine touches infrastructure.

## Examples

Runnable, end-to-end programs live in [`examples/`](examples/):

- [`cookbook`](examples/cookbook) — one small, self-contained recipe per topic:
  events, streaming, approval, policies, declaring tools, observers.

Measurements live in [`benchmarks/`](benchmarks/): each one is a folder with the
program, how to run it, and the reports it writes.

## Documentation

- **The guide** — [`guide/`](guide) explains what the library offers, the
  concepts behind it, and how to do specific things. It is written in Markdown
  and built with [mdBook](https://rust-lang.github.io/mdBook/); run
  `mdbook serve guide` to read it locally, and CI publishes it to GitHub Pages
  from `dev`.
- **Rustdoc** — every crate documents its public surface; the reference is
  generated, not written twice:

  ```sh
  cargo doc --workspace --no-deps --open
  ```

- **Examples** — [`examples/cookbook`](examples/cookbook) is one small recipe per
  topic, each runnable on its own.
- **Contributing** — [`CONTRIBUTING.md`](CONTRIBUTING.md) covers the layout, the
  build and the lint policies.
- **Changelog** — [`CHANGELOG.md`](CHANGELOG.md) records what each version
  changed.

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
