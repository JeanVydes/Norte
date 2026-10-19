# Switch provider without touching the app

The application should never name a vendor. An `Arc<dyn Model>` is built once, at
the edge of the program, from configuration; everything else sees the port.

## Build the model from configuration

```rust,ignore
use std::sync::Arc;

use rustychain_core::{Capabilities, Error, Model, Modality, ToolKind};

fn model_from_env(capabilities: Capabilities) -> Result<Arc<dyn Model>, Error> {
    let model = std::env::var("MODEL").map_err(|_| Error::InvalidInput {
        detail: "set MODEL=provider/model".to_owned(),
    })?;
    let key = std::env::var("API_KEY").map_err(|_| Error::InvalidInput {
        detail: "set API_KEY".to_owned(),
    })?;

    let (provider, id) = model.split_once('/').ok_or_else(|| Error::InvalidInput {
        detail: "MODEL must look like provider/model".to_owned(),
    })?;

    Ok(match provider {
        "openai" => Arc::new(rustychain_openai::presets::openai(id, key, capabilities)?),
        "anthropic" => Arc::new(rustychain_anthropic::presets::anthropic(id, key, capabilities)?),
        "gemini" => Arc::new(rustychain_gemini::presets::gemini(id, key, capabilities)?),
        "compatible" => {
            let base = std::env::var("BASE_URL").expect("set BASE_URL");
            Arc::new(compatible(id, base, key, capabilities)?)
        }
        other => {
            return Err(Error::InvalidInput {
                detail: format!("unknown provider `{other}`"),
            })
        }
    })
}
```

where `compatible` is the `VendorProfile` + `OpenAiChat::new` form from
[Getting started](../getting-started.md#pointing-it-somewhere-else).

Switching providers is now an environment change:

```sh
MODEL=openai/gpt-5 API_KEY=sk-...        cargo run
MODEL=anthropic/claude-sonnet-4-5 API_KEY=sk-ant-... cargo run
MODEL=compatible/deepseek/deepseek-v4-flash-0731 BASE_URL=https://openrouter.ai/api/v1 API_KEY=sk-or-... cargo run
```

## Declare capabilities with the model

Capabilities belong next to the model choice, because they are a property of the
deployment you configured and not of the code. `capabilities_from_json` exists
for exactly that: each adapter crate exports it, so a host can keep the
declaration in a config file or a database.

```rust,ignore
let capabilities = rustychain_openai::capabilities_from_json(&json!({
    "input": ["text", "image"],
    "output": ["text"],
    "tool_kind": "function",
    "streaming": "delta",
    "cache": "automatic"
}))?;
```

Its schema tracks the neutral model and is versioned with it.

## What changes, and what does not

| Changes | Does not change |
| --- | --- |
| The `Arc<dyn Model>` the builder is given. | Your session code, your store, your policies, your tools. |
| `ModelRef` recorded in new commits. | The history already committed: it replays as history. |
| The wire's quirks, handled by the adapter. | `Part`s, `Usage`, `EngineEvent`s — the neutral vocabulary. |
| The route a router picks, if you use one. | Anything above `Model`. |

Because a `ModelRef` is recorded per commit, a session that switched mid-way
keeps a truthful record: each message says which model produced it, and an export
re-imports with the same per-request labels.

## Pinning a route

If you talk to a router, pin the upstream when you need two runs to be
comparable:

```rust,ignore
let mut options = TurnOptions::default();
options.extensions.insert(
    "openai.chat:provider".to_owned(),
    json!({ "order": ["DeepInfra"], "allow_fallbacks": false }),
);
```

That is a routing hint, never a price: what the request actually cost is what the
response reports. See [Usage and cost](../concepts/usage-and-cost.md).
