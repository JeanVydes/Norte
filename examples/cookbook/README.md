# cookbook

Small recipes for specific Norte tasks. Nothing is shared between them, so a
recipe can be read — and copied — without the rest of the crate.

The layout says which layer a recipe needs:

- **The crate root** holds the recipes that use `norte-core` alone — the `direct_*`
  ones speak to the `Model` port or the turn loop without an engine, and `tools`,
  `tools` never calls a model at all.
- **`harness/`** holds the recipes that drive `norte-harness`, whose `Engine` walks
  a session forward a turn at a time. They are the ones that look like an agent:
  a turn, a tool call, an approval, a policy.
- **`media/`** holds the recipes about media: inline bytes, blobs, URLs and the
  vendor-file lifecycle.

A recipe that needs the harness says so by importing it; a host that writes its
own loop copies from the root and never sees the harness.

## Run

```sh
OPENROUTER_API_KEY=sk-or-... \
COOKBOOK_MODEL=deepseek/deepseek-v4.1-flash-1017 \
COOKBOOK_ENDPOINT=https://openrouter.ai/api/v1 \
cargo run -p norte-cookbook --example approval
```

The key comes from the environment; a local `.env` works with
`set -a; . ./.env; set +a`. `tools` needs no key and calls no model: it
declares tools and prints the schema that would go to the provider.

## Recipes

### `norte-core` alone

The recipes in the crate root drive the ports directly. No harness, no session,
no store.

| Example | Topic |
| --- | --- |
| `model` | The `Model` port: one request, every event, nothing committed |
| `turn` | The turn loop by hand: fold the stream, keep the history |
| `tools` | Declaring tools: the macro, the explicit form, and rich `ToolOutput` |
| `layer` | A layer around one call: cache it, rewrite it, or never reach the model |
| `image_in` | A picture in: bytes as a `Part::Media`, and the modality that allows it |
| `image_out` | A picture out: media chunks folded into a part, and where the bytes go |

`model`, `turn`, `layer`, `image_in` and `image_out` call a real model when run;
`tools` needs no key, because it only prints what would go on the wire.

### `norte-harness`

| Example | Topic |
| --- | --- |
| `events` | Reading a turn's event stream |
| `streaming` | Streaming live vs collecting with `finish()` |
| `observer` | Observers and transforms: the side channel that cannot stall a turn |
| `approval` | Human approval: park the turn, decide, resume |
| `policy` | Denying and rewriting tool calls before they run |
| `policy_context` | A policy that decides on what the request already did |
| `agent` | An agent turn: the model edits a file through the host tools |
| `export_import` | A byte-identical restore: the same request before and after a round trip |
| `branching` | Rewind to an ancestor and branch: the shared prefix stays cached |
| `rate_limit` | A tool call the provider refuses, and what the loop does with it |
| `observers` | Observers and stream transforms, on a live turn |

### `media`

| Example | Topic |
| --- | --- |
| `media` | Inline bytes, blobs and URL ingestion in one turn |
| `provider_files` | The vendor-file lifecycle, from upload to replay |

The cost of an agentic session is a measurement rather than a recipe, and it
lives in [`benchmarks/agent-cost`](../../benchmarks/agent-cost). What the
provider's cache was worth across a turn, a tool call, a park and an export is
also a measurement: [`benchmarks/cache-hit`](../../benchmarks/cache-hit).

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key every recipe that calls a model uses (`tools`, `layer` and `image_*` excepted as noted above). |
| `COOKBOOK_MODEL` | *required* | The model the recipes call. |
| `COOKBOOK_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL` the recipes call. |
| `COOKBOOK_PROVIDER` | OpenRouter decides | Pins the upstream provider for `branching`, so two runs of the same recipe stay comparable. |

Every recipe that calls a model talks to a real one, so the model decides what
it does with a prompt: the tool recipes say, in a system message, which tool to
call.

OpenRouter serves a model from one of several upstream providers, and they do
not all implement prompt caching or report it. The cache-read numbers in
`branching` therefore describe the route the run actually got, and the same
model id can report a hit on one run and nothing on the next. `COOKBOOK_PROVIDER`
names the route instead of leaving it to the router; a run that pins one gets a
hard failure when that provider refuses it (a 429 of its own, not a fallback),
which is the honest outcome for a measurement. The `cache-hit` benchmark pins it
for the same reason.

The wiring is ~10 lines of [OpenRouter](https://openrouter.ai) — a
`VendorProfile` with a bearer key, plus the generic `OpenAiChat::new` — because
an `OpenAI`-compatible vendor needs no preset, only a base `URL`. Any other
compatible endpoint is the same code with a different `URL` and key; the native
vendors use their own adapters (`norte-openai::presets`,
`norte-anthropic`, `norte-gemini`).
