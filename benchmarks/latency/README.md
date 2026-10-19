# latency

Time to first token and time per turn, measured twice on the same ask: once by
watching the stream, once by calling `finish()` and looking only at the end.

`finish()` is not a second request — it drains the same stream — but a host that
never reads an event cannot timestamp the first one, so it has no time-to-first
token at all. This benchmark runs both against a live provider every round and
reports the distribution, so the cost of *not* streaming is a number.

Every delta is classified as it arrives: text, reasoning, or something else. A
model that spends its whole output budget thinking has a first token and no
answer, and that is a latency fact the report shows rather than hides
(`note = no text delta arrived`).

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
cargo run -p norte-bench-latency -- --rounds 5
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |
| `BENCH_PAUSE_MS` | `2000` | Wait before each model request, so an asynchronous provider cache can publish the previous prefix. `0` disables it. |

Nothing external is defaulted in the source: a missing variable is an error that
names it. Options: `--rounds <n>`, `--prompt <s>`, `--report <p>`, `--jsonl <p>`,
`--help`.

The wait is taken **before** the timer starts, so neither TTFT nor turn time
includes it. The cache read is reported per round: after the first request, the
same ask should be served from the prefix cache.

## The report

- **Run** — the route, the rounds, the ask.
- **Summary** — per mode: TTFT min/p50, turn time min/p50/max, delta counts and
  the provider's output-token total and cost.
- **Per round** — every measured turn: TTFT, total, delta breakdown, bytes,
  output tokens, tokens per second and cost.
- **Sparklines** — turn time per mode.

Each run also writes `<stem>.jsonl`, `<stem>-rounds.csv` and a manifest beside
the markdown.

## How to read it

- **TTFT only exists if you stream.** The buffered row has an em dash where the
  streaming row has a number; that gap is the point.
- **Time is host wall-clock; tokens and cost are the provider's.** Tokens per
  second is the provider's output count divided by this host's measured time.
- **Variance is normal.** Compare runs that agree on model, provider, endpoint
  and commit.
