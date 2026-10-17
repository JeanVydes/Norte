# react-agent

A streaming `ReAct` agent built on RustyChain and wired to OpenRouter.

It runs a shell command through an approval-gated tool, fetches the
[models.dev](https://models.dev) catalogue with a tool of its own, and then fans
the result out to two specialist models — one briefing the providers, one
briefing the models.

## Run

```sh
OPENROUTER_API_KEY=sk-or-... cargo run -p react-agent
```

The shell tool asks for confirmation before it executes anything; everything
else streams to the terminal as it happens. Reasoning arrives dimmed, tool calls
and their arguments in cyan, tool results summarised, and the final answer as
plain text.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | — | Required. |
| `AGENT_MODEL` | `nvidia/nemotron-3.5-super-vl:free` | The model that drives the tool loop. |
| `PROVIDERS_MODEL` | `nvidia/nemotron-3-super-120b-a12b:free` | The analyst for `providers`. |
| `MODELS_MODEL` | `google/gemma-4-31b-it:free` | The analyst for `models`. |
| `CATALOG_URL` | `https://models.dev/catalog.json` | Where the fetch tool reads from. |

## Layout

| File | Role |
| --- | --- |
| `src/main.rs` | Wiring: engine, model, tools, policies, and the run. |
| `src/tools.rs` | `fetch_catalog`, a tool with runtime state of its own. |
| `src/catalog.rs` | The catalogue cache and the bounded views fed to the analysts. |
| `src/render.rs` | Live console rendering and the approval prompt. |
| `src/branch.rs` | The single-turn analysis each specialist model performs. |

## Notes

The engine runs on the in-memory `Store` and `BlobStore` that ship with
`rustychain-core`; for durability, hand the builder your own.

The catalogue is several megabytes, so it never reaches a model whole: the agent
sees a summary and each analyst gets a bounded projection of its half. The tool
keeps the full document host-side in a shared cache, which is what makes the
fan-out possible.
