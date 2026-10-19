# schema-friction

How much friction a schema has, as a number: the requests and the rejected calls
it takes a model to make one valid call.

When a tool call does not satisfy its schema, the engine commits the failure as a
tool error and asks the model again — so a schema that is hard to satisfy is paid
for in requests, tokens and money, not just in an error message. This benchmark
runs four shapes against a deliberately awkward ask and counts what happened:

- **flat** — one string field, no trap;
- **enum** — one enum field (`low`, `medium`, `high`), with an ask that insists
  on `urgent`, which is not in the enum;
- **nested** — a nested guest object and a number under `deny_unknown_fields`,
  with an ask that insists on an extra `floor` field the schema does not declare;
- **tagged** — an internally tagged enum (`command: {kind: ...}`), the shape the
  `command` tool uses, with an ask that insists on a plain string.

The asks are traps on purpose. What the model does with a trap is the
measurement; whether it always falls for it is not, and a shape that needs zero
extra requests is reported as exactly that.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4.1-flash-1017
cargo run -p norte-bench-schema-friction -- --rounds 5
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |
| `BENCH_PAUSE_MS` | `2000` | Wait before each model request, so an asynchronous provider cache can publish the previous prefix. `0` disables it. |

Options: `--rounds <n>`, `--report <p>`, `--jsonl <p>`, `--help`.

The pause is included in the elapsed column. It gives a tool-loop request or a
repeat round a chance to hit the prefix cache instead of racing its publication.

## The report

- **Run** — the route, the rounds, the shapes.
- **Shapes** — per schema: requests, calls made, calls rejected by the schema,
  calls accepted, rounds answered, tokens and cost.
- **Rejected calls** — the distinct rejection texts, so a schema error is not
  confused with a policy denial or a compiler error.
- **Per round** — the same numbers for every individual round, including the
  rejection text.
- **Bars** — rejected calls per schema.

Each run also writes `<stem>.jsonl`, `<stem>-schemas.csv`, `<stem>-rounds.csv`
and a manifest beside the markdown.

## How to read it

- **A rejected call is a whole extra request** — the transcript, the schemas and
  the failed arguments again. That is why the count is a cost, and why the
  report keeps the error text rather than only the boolean.
- **The tagged row is the discriminating one.** A scalar enum field and an
  internally tagged object are different schema problems; the latter is the
  shape the `command` tool uses.
- **Zero rejects is only a schema finding if the tool was called.** The
  `called` column counts the rounds in which the model actually invoked the
  tool. A row that answered without calling did not exercise the schema, so its
  zero rejects says nothing about the shape.
- **The bill is the provider's.** Cost is summed from the `usage.cost` the
  response carried. The pause is included in the elapsed column.
