# Getting started

## Install

```toml
[dependencies]
norte = { version = "0.0.2", features = ["openai"] }
futures-util = "0.3"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The `norte` facade re-exports core types and enabled adapters. You can also
depend on `norte-core` and a provider crate directly. Provider features are
`openai` (enabled by default), `anthropic`, and `gemini`.

## Make a model request

Configure a provider adapter with a model reference, endpoint profile, and
capabilities, then send a `ModelRequest`:

```rust,ignore
use futures_util::StreamExt;
use norte_core::{Message, Model, ModelRequest};

let request = ModelRequest::new(
    model.reference().clone(),
    vec![Message::user("Hello!")],
);
let mut stream = model.stream(request).await?;

while let Some(event) = stream.next().await {
    handle_model_event(event?);
}
```

The concrete adapter determines how the request is encoded and sent. The stream
contains neutral Norte events described in the `norte_core` Rustdoc.

## What the host owns

Norte returns model events and provides tool declarations and execution
primitives. Your application decides how to maintain history, invoke tools,
continue a tool-using interaction, handle retries and cancellation, and save
state. Norte does not create sessions or persist conversation history.
