# cookbook

Small recipes for specific engine tasks. Each one is a single file, runnable on
its own, and wires its own engine: nothing is shared between them, so a recipe
can be read — and copied — without the rest of the crate.

## Run

```sh
OPENROUTER_API_KEY=sk-or-... cargo run -p rustychain-cookbook --example approval
```

The key comes from the environment; a local `.env` works with
`set -a; . ./.env; set +a`. `tools` and `executors` need no key and no model:
the first declares tools and prints the schema that would go to the provider,
the second calls the shipped executors directly.

## Recipes

| Example | Topic |
| --- | --- |
| `events` | Reading a turn's event stream |
| `streaming` | Streaming live vs collecting with `finish()` |
| `approval` | Human approval: park the turn, decide, resume |
| `policy` | Denying and rewriting tool calls before they run |
| `policy_context` | A policy that decides on what the request already did |
| `tools` | Declaring tools: macro, explicit, and rich `ToolOutput` |
| `agent` | An agent turn: the model edits a file through the host tools |
| `executors` | Calling `read`, `write`, `edit` and `command` directly |
| `observers` | Observers and stream transforms |

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | — | Required by every recipe that calls a model (`tools` and `executors` need none). |
| `COOKBOOK_MODEL` | `nvidia/nemotron-3.5-super-vl:free` | The model the recipes call. |

Every recipe that calls a model talks to a real one, so the model decides what
it does with a prompt: the tool recipes say, in a system message, which tool to
call.

The wiring is ~10 lines of [OpenRouter](https://openrouter.ai) — a
`VendorProfile` with a bearer key, plus the generic `OpenAiChat::new` — because
an `OpenAI`-compatible vendor needs no preset, only a base `URL`. Any other
compatible endpoint is the same code with a different `URL` and key; the native
vendors use their own adapters (`rustychain-openai::presets`,
`rustychain-anthropic`, `rustychain-gemini`).
