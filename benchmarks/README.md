# benchmarks

A benchmark runs the real engine against a real model, prints what happened as it
happens, and writes the result down as markdown under its own `results/` — so a
number can be audited later without replaying the run that produced it.

```
benchmarks/
  README.md            # this file
  support/             # shared reporting, route and measurement support (norte-bench-support)
  agent-cost/          # one folder per benchmark
  cache-hit/           # Cargo.toml, src/main.rs, README.md, results/
  ...
```

Every benchmark is built on `norte-bench-support`: the markdown report, the JSONL
trail, the CSV tables, the run manifest, the route and the size accounting all
come from there, so two benchmarks write their results the same way.

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
model, the provider, the endpoint, the pause, the budget and the commit. A
different route is a different measurement, and run-to-run variance on the same
route is normal — the model decides how many requests a task takes.

## Pacing the provider cache

A provider's prefix cache is written asynchronously. Two requests fired back to
back can both miss a prefix that would have been readable a moment later, which
makes a cache measurement look like a provider bug when it is only the pace of
the benchmark. Every live benchmark therefore waits `BENCH_PAUSE_MS` before each
model request. The default is **2000 ms**; `BENCH_PAUSE_MS=0` restores the
back-to-back pace for a host that wants to measure exactly that.

The pause is part of the protocol: it is written into the route line of every
report and into the manifest. `latency` takes the wait before it starts its
timer, so neither TTFT nor turn time includes it. The other live benchmarks
include it in any elapsed column and say so. A result produced before the pause
was introduced is not comparable with one produced after it.

`agent-cost`, `cache-hit`, `latency`, `long-context`, `schema-friction`,
`token-counting` and `tool-loop` all report the provider's `cache read`, so the
effect of the pace is visible rather than assumed.

## Index

| Benchmark | What it measures | State |
| --- | --- | --- |
| [`agent-cost`](agent-cost) | What a coding agent costs on an existing crate, per request, per task and in total | runs |
| [`cache-hit`](cache-hit) | What survives a turn, a tool call, a park and an export: cache reads, the bill and the latency of each request | runs |
| [`latency`](latency) | Time to first token and time per turn, streaming against `finish()` | runs |
| [`long-context`](long-context) | A long session kept whole against the same session with its history compacted | runs |
| [`roundtrip-fidelity`](roundtrip-fidelity) | Export and import over many sessions, four wires, byte for byte | runs |
| [`schema-friction`](schema-friction) | How many requests a model spends getting a tool call the schema accepts | runs |
| [`token-counting`](token-counting) | The provider's counters against the engine's accounting | runs |
| [`tool-loop`](tool-loop) | How requests and cost grow with declared tools and transcript size | runs |
| [`transport-retry`](transport-retry) | What a rate limit or a timeout does under a host retry policy, and what the retries cost | runs |

`latency`, `long-context`, `schema-friction`, `token-counting` and `tool-loop`
call a live provider and need a key. `roundtrip-fidelity` and `transport-retry`
are offline and deterministic, and run in CI.

Every live benchmark now has a canonical run with the 2 s pause. Compare only
runs that agree on route, model and `BENCH_PAUSE_MS`; the old unpinned results
are in git history and are not comparable.

## Proposed

No benchmark is currently proposed. A candidate is added here with the measurement
it would make and why the number would be worth paying for; it moves to the index
when it is implemented and run.
