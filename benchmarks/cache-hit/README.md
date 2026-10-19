# cache-hit

What the provider's prompt cache was actually worth across a turn, a tool call,
a park and an export — measured in the provider's own tokens, bill and wall-clock.

Two things are being watched, and they belong to different owners:

- **The bytes are the library's.** Reconstruction is append-only and
  deterministic, so every request starts with exactly the message prefix of the
  one before it. The benchmark records the exact body the adapter sent and, for
  each request, the number of messages shared with the request before it, whether
  they are byte-identical, and their digest.
- **The tokens are the provider's.** `Usage::cache_read_tokens` is what the
  provider says it read from its cache; that is the only number that proves a hit.
  An identical prefix with no cache read is the provider deciding — a short
  prefix, an expired entry, a model that does not cache at all.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=DeepInfra          # optional: pins the upstream
cargo run -p norte-bench-cache-hit
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream through the router, so two runs stay comparable. A routing hint, never a price. |

Nothing external is defaulted in the source: a missing variable is an error that
names it. `--help` prints this, and `--report <path>` writes the report somewhere
other than `results/`.

The prompt has to be past the provider's caching floor — often about a thousand
tokens — so the system message is long on purpose. Whether a hit is *reported* is
the provider's: a provider that does not keep a cache, or does not report cache
usage, will show identical prefixes and no cache reads. That is a result, not a
failure.

## The four phases

1. **The normal case** — two turns; the second re-sends the conversation.
2. **After a tool call** — the call and its result join the prefix at the end.
3. **After human approval** — the turn parks, and the resume re-sends what the
   model already answered.
4. **After export and import** — the conversation is restored into a fresh
   engine under the same id, and asked to carry on. The transcript is new memory;
   the bytes must not be.

## The report

Each run writes itself a markdown report under `results/`, named after the moment
and the route:

- **The run** — when, which model, which provider, the endpoint and the commit.
- **Reproduce** — the environment block for that exact run.
- **Turns** — the wall-clock of each `finish()`.
- **Requests** — every provider request, read from the assistant message it
  committed, with input, output, cache read, cache write, hit rate and the
  provider's `usage.cost`. The request inside a tool loop is counted too.
- **Prefix audit** — for every request, the messages shared with the previous
  request, whether they are byte-identical, and their digest.
- **What this shows** — whether a live hit was reported, and on how many
  requests.

The report is written by the run that measured it, so it cannot drift from what
happened.

## How to read it

- **A cache read is only meaningful next to the prefix audit.** A hit on a
  request whose prefix is not byte-identical would be a library bug; a miss on an
  identical prefix is the provider's call.
- **The bill is the provider's.** The benchmark sums the `usage.cost` each
  response carries and never applies a rate. A request without a cost is named,
  not priced.
- **The cache is inside the input on `OpenAI`-compatible wires.** `input_tokens`
  includes the cache-read tokens, so the hit rate is `cache_read / input` and the
  saving is already in the provider's `cost`.
- **Pin the provider for a comparison.** With `OpenRouter` free to route, two
  turns of the same conversation can land on different upstreams, each with its
  own cache; the hit then measures routing, not the prefix.
