# schema-friction

How much friction a schema has, as a number: the requests and the rejected calls
it takes a model to make one valid call.

When a tool call does not satisfy its schema, the engine commits the failure as a
tool error and asks the model again — so a schema that is hard to satisfy is paid
for in requests, tokens and money, not just in an error message. This benchmark
runs three shapes against a deliberately awkward ask and counts what happened:

- **flat** — one string field, no trap;
- **enum** — one enum field (`low`, `medium`, `high`), with an ask that insists
  on `urgent`, which is not in the enum;
- **nested** — a nested guest object and a number under `deny_unknown_fields`,
  with an ask that insists on an extra `floor` field the schema does not declare.

The asks are traps on purpose. What the model does with a trap is the
measurement; whether it always falls for it is not, and a shape that needs zero
extra requests is reported as exactly that.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
cargo run -p norte-bench-schema-friction -- --rounds 3
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |

Options: `--rounds <n>`, `--report <p>`, `--jsonl <p>`, `--help`.

## The report

- **Run** — the route, the rounds, the shapes.
- **Shapes** — per schema: requests, calls made, calls rejected by the schema,
  calls accepted, rounds answered, tokens and cost.
- **Per round** — the same numbers for every individual round.
- **Bars** — rejected calls per schema.

Each run also writes `<stem>.jsonl`, `<stem>-schemas.csv`, `<stem>-rounds.csv`
and a manifest beside the markdown.

## How to read it

- **A rejected call is a whole extra request** — the transcript, the schemas and
  the failed arguments again. That is why the count is a cost.
- **Zero rejects is a finding, not a failure.** A schema the model gets right the
  first time is the outcome a schema author wants.
- **The bill is the provider's.** Cost is summed from the `usage.cost` the
  response carried.
