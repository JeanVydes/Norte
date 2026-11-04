# Norte

**Provider-agnostic infrastructure for AI agents in Rust.** One core, three
first-class provider SDKs, and a streaming, durable engine.

Nothing in your application names a vendor, and no model catalogue is baked in:
you declare what your models can do, and the engine does the rest — every call
under rules you write, every turn in a history you can audit.

## What you get

- A **model port** (`Arc<dyn Model>`): swap the provider by swapping one
  `Arc`, not the application.
- **Durable sessions**: a session is a tree of commits in a `Store`; a turn
  ends in exactly one commit — never in between.
- **Streaming tools**: tools run, stream their output and report back, with
  the host deciding what a call may attempt.
- **Provider-reported usage and cost**: no rate table, no invented prices.

## What it is not

- Not a model catalogue: capabilities are declared by you, and anything a
  wire reports that the neutral model has no field for is kept, not
  interpreted.
- Not an agent framework: no built-in ReAct loop, planner or prompt library.
  The loop around the engine is your code.
- Not tied to a cloud: persistence is two traits (`Store`, `BlobStore`)
  with in-memory implementations ready to use.

## Where to go

- New here: [Getting started](getting-started.md).
- Want the vocabulary first: [Sessions and turns](concepts/sessions-and-turns.md),
  then [Streaming](concepts/streaming.md).
- Solving something concrete: the [guides](guides/switch-provider.md).
- Looking for a type: [Crates](reference/crates.md) and
  [Glossary](reference/glossary.md); the full API is in rustdoc
  (`cargo doc --workspace --no-deps --open`).
- Auditing a promise: [Requirements](reference/requirements.md) carries every id
  with the code and the CI job that enforce it.
