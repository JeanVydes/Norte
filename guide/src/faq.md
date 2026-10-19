# FAQ

## Why is there no `cost` field on `Usage`?

Because the library does not price anything, and a price table would be wrong the
first time a request was routed to a different upstream. When a provider reports a
price, it lands in `Usage::vendor` under the provider's own key — `cost`, on
`OpenRouter` — and summing that field gives a bill the library never guessed. See
[Usage and cost](concepts/usage-and-cost.md).

## Why is there no model catalogue?

A model's context window, modalities and price are a property of a deployment, not
of a wire. Declaring them (`Capabilities`) makes the engine check requests before
the network; looking them up would make the library stale. The one place a
catalogue belongs is the host, which can keep it in config.

## Which providers are supported?

OpenAI (`Responses` and `Chat Completions`), Anthropic (`Messages`), Gemini
(`generateContent`), and any `OpenAI`-compatible endpoint through the chat wire
with its own base `URL` — OpenRouter, Groq, Together, vLLM, llama.cpp and the
rest.

## Can I run against a local model?

Yes, if it speaks the `OpenAI` chat protocol (vLLM, llama.cpp's server, Ollama's
compatibility endpoint). Point `VendorProfile` at `http://localhost:8000/v1` and
use `OpenAiChat`. Declare capabilities honestly — a local model that does not
stream should be declared `StreamingGranularity::Disabled`.

## Does the library phone home?

No. There is no telemetry, no analytics, no licence check and no update ping. The
only network traffic is to the model endpoint you configure; storage, tools and
secrets all belong to the process you run.

## Why so many crates?

So that a dependency rule can be enforced: core depends on nothing internal, and
no adapter depends on another. `scripts/check-dependencies.sh` fails the build if
that is violated. It also means you compile one provider, not three.

You do not have to depend on them one by one. `norte` is the entrypoint and
re-exports all of them — the core at the root, each other crate as a module of its
own name (`norte::openai`, `norte::tools`, …) — so one line in `Cargo.toml` is
enough. Depend on the individual crates when you would rather be exact about what
you compile.

## Is the behavioural spec in the repository?

Yes: [Requirements](reference/requirements.md) is the ledger behind the library —
every requirement with its id, the code that enforces it and the suite or CI job
that checks it. The ids in source comments and workflow names resolve there. The
code is the source of truth; the ledger is the requirement it implements.

## My turn ended with an error, but the requests were billed. Why?

Two cases, both deliberate:

- **The iteration budget ran out.** `max_iterations` (16 by default) bounds the
  model requests in a turn. A coding agent needs more, so a host raises it — the
  benchmark uses 32. Every request made before the error was committed and billed,
  which is why a cost meter keeps counting past the failure.
- **The store raced.** A commit built from a stale version fails with
  `CommitConflict` rather than overwriting another worker's turn. Reload and
  retry.

## Why is my cache not being hit?

Three usual causes: a part of the request changed above the boundary (a
timestamp, a re-ordered tool list, a different system message), the provider
simply does not cache on that route, or the route changed between requests. The
`CacheClass` on messages is where boundaries come from, and
`cache_boundaries`/`PrefixInvalidated` are how you see what happened. A router
may serve the same model from different upstreams, and not all of them cache.

## Does it stream to a browser?

The engine is transport-agnostic: `EngineStream` is a Rust `Stream`, and it is up
to you to forward events over SSE or a WebSocket. There is no HTTP server in the
library on purpose.

## Is there a built-in agent loop / planner?

No. The engine gives you a durable, streaming, policied turn with tools,
approval, cache awareness and cost visibility. The loop around it — when to call
the model again, what to tell it, when to stop — is application code. The `agent`
recipe in `examples/cookbook` and `benchmarks/agent-cost` are two small loops built
on top of it.

## Can I attach an image or audio?

Yes, within what you declared. Media has a `Part`, a `Modality`, a blob store for
the large bytes (`inline_threshold` decides when they leave the history), and
`norte-media` for preprocessing an image down to the provider's limits
(`ImagePreprocessor`: max dimension, max bytes, output format, quality).

## How do I know two runs are comparable?

Record what the run was: model, provider, endpoint, parameters and the commit of
the code. `benchmarks/agent-cost` writes exactly that into every report, which is
why its results are readable months later. See
[benchmarks](https://github.com/JeanVydes/Norte/tree/dev/benchmarks).
