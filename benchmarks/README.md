# benchmarks

A benchmark runs the real engine against a real model, prints what happened as it
happens, and writes the result down as markdown under its own `results/` — so a
number can be audited later without replaying the run that produced it.

```
benchmarks/
  README.md            # this file
  agent-cost/          # one folder per benchmark
    Cargo.toml
    src/main.rs
    README.md          # what it measures, how to run it, how to read it
    results/           # one file per run, written by the run
```

## What a benchmark is here

- **The provider's own numbers.** Every figure is the provider's: the `usage.cost`
  a response reports, the token counters it reports. No price table lives in this
  repository, because the route that served a request is the router's decision,
  and a rate written down here would be the list price of a route that may not
  have been the one used. A request whose payload carries no cost is named and
  counted, not priced.
- **Reproducible from its README.** The model, the endpoint and any provider pin
  come from the environment; nothing external is baked into the source. The
  report records the route, the budget and the commit the run started from.
- **Self-documenting.** Each run leaves a report under `results/`, written by the
  run itself and complete enough to audit the totals.
- **Not a test.** A benchmark calls a paid model, so it does not run in CI. The
  crates are still built and linted there like everything else.

## How to read a result

Compare only runs that agree on the lines in the report's *The run* table: the
model, the provider, the endpoint, the budget and the commit. A different route
is a different measurement, and run-to-run variance on the same route is normal —
the model decides how many requests a task takes.

## Index

| Benchmark | What it measures | State |
| --- | --- | --- |
| [`agent-cost`](agent-cost) | What a coding agent costs on an existing crate, per request, per task and in total | runs |

## Proposed

| Benchmark | What it would measure | Why it is worth the money |
| --- | --- | --- |
| `cache-hit` | The same long prompt with and without the provider's cache, over several rounds: the bill and the latency of each | The cache is the largest lever on a long transcript, and only the provider's own numbers say what it was worth. |
| `tool-loop` | How requests and cost grow with the number of declared tools and with the size of the transcript | The tool declarations are re-sent on every request, so they are paid for once per iteration whether or not they are used. |
| `schema-friction` | How many requests a model spends guessing at a tool schema before it sends a shape the policy accepts | A schema is the cheapest thing to fix and the dearest to leave: the shape a tool declares is re-sent and re-read until the model gets it right. |
| `long-context` | A long session kept whole against the same session with its history compacted | A compaction policy is a claim about money; this is how it is checked. |
| `latency` | Time to first token and time per turn, streaming against `finish()` | The streaming contract's cost in wall-clock, not money. |
| `transport-retry` | What a rate limit or a timeout does under the transport policy, and what the retries cost | The route refuses; the engine must not paper over it, and a retry that nobody counted is part of the bill. |
| `roundtrip-fidelity` | Export and import over many sessions, byte for byte | Not money: the byte-identity claim, measured at scale instead of asserted. |
| `token-counting` | The provider's counters against what the engine reports | The meters have to agree before any bill built on them means anything. |
