# RustyChain

**Provider-agnostic infrastructure for LLM agents in Rust.** One core, three
first-class provider SDKs, and a streaming engine built to ship.

Write your agent once and run it on OpenAI, Anthropic or Gemini — or on any
`OpenAI`-compatible endpoint — without touching a line of application code. No
provider leaks into your application, and no model catalogue is baked in: you
declare what your models can do and the library does the rest.

## The mental model

Five ideas, and everything else in this guide is a detail of one of them.

- **A model is a port.** `Arc<dyn Model>` is the only thing the engine knows how
  to call. A wire adapter (`rustychain-openai`, `-anthropic`, `-gemini`) is one
  implementation; a scripted model in a test is another. Swapping the provider is
  swapping one `Arc`, not rewriting the application.
- **A session is durable, a turn is atomic.** A `Session` is a tree of commits in
  a `Store`. A turn ends in exactly one commit: the session is either at the
  version before it or the version after it, never in between. A tool loop is
  several turns, each committed and each billed on its own.
- **Everything the model said is kept.** Parts, provider reasoning blocks the
  neutral model has no field for, token counters, cache markers — kept verbatim
  and replayed unchanged, so a session can be exported and re-imported
  byte-for-byte and its provider cache prefix survives.
- **The host decides what a tool may do.** Policies see every tool call before it
  runs and can allow it, deny it, transform its arguments or require a human.
  The engine never executes a call a policy refused.
- **The engine never prices anything.** `Usage` carries the counters a provider
  reported and, verbatim, whatever else its payload carried — including a price,
  if it reported one. A rate table is not something this library maintains.

## What it is not

- Not a model catalogue. There is no table of model ids, context windows or
  prices in the repository: capabilities are declared by the host, and anything a
  wire reports that the neutral model has no field for is kept, not interpreted.
- Not an agent framework. There is no built-in ReAct loop, no planner and no
  prompt library. What the engine gives you is a durable, streaming, policied
  turn; the loop around it is your code.
- Not tied to a cloud. Persistence is two traits — `Store` and `BlobStore` — with
  in-memory implementations that are ready to use and nothing else.

## Where to go

- New here: [Getting started](getting-started.md).
- Want the vocabulary first: [Sessions and turns](concepts/sessions-and-turns.md),
  then [Streaming](concepts/streaming.md).
- Solving something concrete: the [guides](guides/switch-provider.md).
- Looking for a type: [Crates](reference/crates.md) and
  [Glossary](reference/glossary.md); the full API is in rustdoc
  (`cargo doc --workspace --no-deps --open`).
