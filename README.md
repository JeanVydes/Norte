<p align="center">
  <img src="https://raw.githubusercontent.com/JeanVydes/Norte/main/assets/banner.jpg" alt="Norte" width="100%">
</p>

# Norte

<p align="center">
  <a href="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml"><img src="https://github.com/JeanVydes/Norte/actions/workflows/rust.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/JeanVydes/Norte/blob/main/LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT"></a>
  <img src="https://img.shields.io/badge/MSRV-1.85-blue.svg" alt="MSRV: 1.85">
  <img src="https://img.shields.io/badge/edition-2024-blue.svg" alt="Edition: 2024">
</p>

**Provider-neutral model and tool APIs for Rust.** Norte gives applications a
shared message model, typed capabilities, provider adapters, and streaming
events. It also offers optional tool, media, and MCP integrations. Your
application controls conversation history, tool loops, sessions, and storage.

## Features

- Stream model responses through one provider-neutral API.
- Use OpenAI, Anthropic, and Google adapters, or connect OpenAI-compatible
  endpoints through the OpenAI adapter.
- Declare model capabilities and supported parameters explicitly.
- Add tools, image input and output, and MCP support through opt-in features.
- Keep runtime policy and conversation persistence in your application.

Norte does not include a model catalogue. Applications supply model IDs and
capabilities from their own configuration or trusted catalogue.

## Install

Add Norte to your `Cargo.toml`:

```toml
[dependencies]
norte = { git = "https://github.com/JeanVydes/Norte", branch = "main", features = ["openai"] }
futures-util = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The `openai` feature is enabled by default. Add `anthropic` or `gemini` for
those providers. Optional integrations include `tools`, `media`, and `mcp`.
The `full` feature enables all provider and integration features.

## Make a request

This program reads `OPENAI_API_KEY`, sends one request to OpenAI, and prints
the stream events. Create a binary crate, add the dependencies above to its
`Cargo.toml`, save the complete program as `src/main.rs`, set
`OPENAI_API_KEY` in your environment, and run `cargo run`.

```rust
use std::error::Error;

use futures_util::StreamExt;
use norte::{Capabilities, Message, Modality, Model, ModelRequest};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let api_key = std::env::var("OPENAI_API_KEY")?;
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = norte::presets::openai("gpt-5", api_key, capabilities)?;
    let request = ModelRequest::new(
        model.reference().clone(),
        vec![Message::user("What is a prompt cache?")],
    );

    let mut stream = model.stream(request).await?;
    while let Some(event) = stream.next().await {
        println!("{:?}", event?);
    }

    Ok(())
}
```

The [model example](examples/cookbook/examples/model.rs) shows the same flow
with an explicit endpoint profile. The [cookbook](examples/cookbook/README.md)
also includes tool loops, chat history, image input and output, and scripted
models for tests.

## Crates

| Crate | Purpose |
| --- | --- |
| [`norte`](norte/) | Facade with optional provider and helper features. |
| [`norte-core`](norte-core/) | Messages, model requests, events, capabilities, and tool contracts. |
| [`norte-openai`](norte-openai/) | OpenAI Responses and Chat Completions adapters. |
| [`norte-anthropic`](norte-anthropic/) | Anthropic Messages adapter. |
| [`norte-gemini`](norte-gemini/) | Google Gemini adapter. |
| [`norte-http`](norte-http/) | Shared HTTP transport and vendor profiles. |
| [`norte-tools`](norte-tools/) | Optional built-in tools. |
| [`norte-mcp`](norte-mcp/) | Optional MCP client integration. |
| [`norte-testing`](norte-testing/) | Scripted models and test support. |

## Documentation

- [User guide](https://jeanvydes.github.io/Norte/)
- [API reference](https://docs.rs/norte/)
- [Contributing](CONTRIBUTING.md)
- [Changelog](CHANGELOG.md)
- [Cookbook](examples/cookbook/README.md)
- [Benchmarks](benchmarks/README.md)

Run the workspace checks with `cargo fmt --all -- --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace`.

## License

MIT. See [LICENSE](LICENSE).
