# tool-loop

What a tool loop costs, and what widens it.

A tool turn is not one request. The model asks for a tool, the engine runs it,
and the model is asked again with the result — so a turn is as many provider
requests as the conversation takes, and every request carries the whole
transcript plus every declared tool schema. This benchmark pins the ask and
varies the two things a host controls:

- **how many tools are declared** — one, or five including four fillers that are
  never called; and
- **how big the transcript already is** — zero, or a few prior turns.

The tool that is used is a real `word_count`; the filler tools exist only to
widen the declared surface. That is the point: a tool that is declared but never
called is still sent on every request.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
cargo run -p norte-bench-tool-loop -- --rounds 1 --prior 3
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |
| `BENCH_PAUSE_MS` | `2000` | Wait before each model request, so an asynchronous provider cache can publish the previous prefix. `0` disables it. |

Options: `--rounds <n>`, `--prior <n>`, `--report <p>`, `--jsonl <p>`, `--help`.
The matrix crosses declared tools (1 vs 5) with transcript size (0 vs `--prior`).

The pause is included in the median turn time. It matters most for the second
request of a tool loop, which otherwise follows the first by no more than the
tool's local execution time.

## The report

- **Run** — the route, the rounds, the ask.
- **Configuration** — per matrix point: schema bytes, first-request bytes,
  requests, tool calls, tool errors, input/output/cache tokens, cost and the
  median turn time.
- **Requests** — every provider request the loop made, with the usage the
  provider reported for it and a digest of its projection.
- **Bars** — declared tools per configuration.

Each run also writes `<stem>.jsonl`, `<stem>-configs.csv`,
`<stem>-requests.csv` and a manifest beside the markdown.

## How to read it

- **The schema is re-sent, not re-declared once.** Read the `bytes` column per
  request beside `input tokens`: that is the schema plus the transcript being
  paid for again each time the model is asked.
- **The request count is the lever.** More loop iterations means the whole
  prefix is billed again.
- **The bill is the provider's.** The benchmark sums the `usage.cost` field the
  response carried; it never applies a rate.
- **The cache read column is also a routing measurement.** Pin `BENCH_PROVIDER`:
  with a free router, two requests of the same loop can land on different
  upstreams, each with its own cache. The pause gives a pinned cache the moment
  it needs; it cannot make a different upstream share one.
