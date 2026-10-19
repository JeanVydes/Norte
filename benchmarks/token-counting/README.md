# token-counting

The provider's counters against the engine's accounting: does every request that
reached the provider leave exactly one usage record, and does that record keep
everything the provider sent?

A host budgets from the numbers the engine commits, so a lost or duplicated usage
record is a budget that is wrong. This benchmark runs a multi-turn conversation
and reconciles three views:

1. **The requests** — counted by a recording model, one per call the engine
   actually made.
2. **The engine's records** — one per committed assistant message: the usage the
   engine normalized out of the provider's payload.
3. **The provider's payload** — the `vendor` map the engine keeps verbatim,
   listed key by key, so a counter the engine did not model is visible rather
   than lost.

The reconciliation is a verdict, not a vibe: `one record per request` either
holds or it does not.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
cargo run -p norte-bench-token-counting -- --turns 3
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |
| `BENCH_PAUSE_MS` | `2000` | Wait before each model request, so an asynchronous provider cache can publish the previous prefix. `0` disables it. |

Options: `--turns <n>`, `--report <p>`, `--jsonl <p>`, `--help`.

The pause is part of the measurement protocol. The session also carries a long,
fixed system message: the prefix has to clear the provider's caching floor before
the cache counters in the reconciliation mean anything. With the pause, the later
turns should report cache reads.

## The report

- **Run** — the route, the turns and the verdict.
- **Reconciliation** — requests against usage records, complete records, the
  summed counters, the derived total, the cost, and the provider keys kept
  verbatim.
- **Per record** — normalized counters (input, output, reasoning, cache read,
  cache write), the derived total, the cost and the provider's own keys.

Each run also writes `<stem>.jsonl`, `<stem>-records.csv` and a manifest beside
the markdown.

## How to read it

- **The derived total is input plus output** — what a budget should be built on.
- **The provider's keys are shown verbatim.** A counter this crate does not model
  still appears in the record, so a change in the provider's payload is visible
  rather than silent.
- **`unpriced records`** counts responses whose payload carried no cost. They are
  named, never priced from a table.
