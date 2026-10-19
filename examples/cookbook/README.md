# cookbook

Small recipes for specific engine tasks. Each one is a single file, runnable on
its own, and wires its own engine: nothing is shared between them, so a recipe
can be read — and copied — without the rest of the crate.

## Run

```sh
OPENROUTER_API_KEY=sk-or-... \
COOKBOOK_MODEL=deepseek/deepseek-v4-flash-0731 \
COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
cargo run -p norte-cookbook --example approval
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
| `cache` | What the provider really read: cache hits across a turn, a tool call, a park and an export |
| `export_import` | A byte-identical restore: the same request before and after a round trip |
| `branching` | Rewind to an ancestor and branch: the shared prefix stays cached |

The cost of an agentic session is a measurement rather than a recipe, and it
lives in [`benchmarks/agent-cost`](../../benchmarks/agent-cost).

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key every recipe that calls a model uses (`tools` and `executors` need none). |
| `COOKBOOK_MODEL` | *required* | The model the recipes call. |
| `COOKBOOK_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL` the recipes call. |
| `COOKBOOK_PROVIDER` | OpenRouter decides | Pins the upstream provider for `cache` and `branching`, so two runs of the same recipe stay comparable. |

Every recipe that calls a model talks to a real one, so the model decides what
it does with a prompt: the tool recipes say, in a system message, which tool to
call.

OpenRouter serves a model from one of several upstream providers, and they do
not all implement prompt caching or report it. The cache-read numbers in `cache`
and `branching` therefore describe the route the run actually got, and the same
model id can report a hit on one run and nothing on the next. `COOKBOOK_PROVIDER`
names the route instead of leaving it to the router; a run that pins one gets a
hard failure when that provider refuses it (a 429 of its own, not a fallback),
which is the honest outcome for a measurement.

The wiring is ~10 lines of [OpenRouter](https://openrouter.ai) — a
`VendorProfile` with a bearer key, plus the generic `OpenAiChat::new` — because
an `OpenAI`-compatible vendor needs no preset, only a base `URL`. Any other
compatible endpoint is the same code with a different `URL` and key; the native
vendors use their own adapters (`norte-openai::presets`,
`norte-anthropic`, `norte-gemini`).
