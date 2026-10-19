# long-context

A long session kept whole against the same session with its history compacted:
what the context costs, and what compaction saves at request time.

A compaction policy is a claim about money. Drop or summarize the old history and
every later request gets cheaper — but the thing it spends is the **prefix**, the
part of the transcript a provider re-reads, re-bills and may cache on every
request. This benchmark builds one deterministic, host-authored set of notes and
sends it two ways:

- **whole** — the full transcript, every exchange, as the session's history.
- **compacted** — a host-written summary of the same notes, as the history.

Each arm then asks the same question and the same follow-up, so the two arms
differ only in the context they carry. Every figure is the provider's own: input
and output tokens, cache reads and writes, the `usage.cost` it reported, and the
wall-clock of each turn. The projection of every request is recorded, and the
warm request's shared prefix is compared with the cold one byte for byte.

This is a **cost comparison, not a quality comparison**: the host-written summary
already names the version and the risk the questions ask for, so the compacted
arm has an informational advantage by construction. The answers are quoted to
show what the run produced, not to score it. The output budget is 1,024 tokens;
a request that commits no text is shown as such, not silently omitted.

## Run

```sh
export OPENROUTER_API_KEY=sk-or-...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4.1-flash-1017
cargo run -p norte-bench-long-context -- --turns 32 --lines 8
```

| Variable | Default | Purpose |
| --- | --- | --- |
| `OPENROUTER_API_KEY` | *required* | The key for the endpoint. |
| `BENCH_ENDPOINT` | *required* | The `OpenAI`-compatible base `URL`. |
| `BENCH_MODEL` | *required* | The model id. |
| `BENCH_PROVIDER` | the router decides | Pins the upstream so two runs stay comparable. |
| `BENCH_PAUSE_MS` | `2000` | Wait before each model request, so an asynchronous provider cache can publish the previous prefix. `0` disables it. |

Options: `--turns <n>` (exchanges in the notes, default 32), `--lines <n>` (lines
per exchange, default 8), `--report <p>`, `--jsonl <p>`, `--help`.

The pause is included in the elapsed columns. It matters here because the warm
follow-up is exactly the request that should reuse the cold request's prefix.

Nothing external is defaulted in the source: a missing variable is an error that
names it. `--report` and `--jsonl` write elsewhere than `results/`.

## The report

- **Run** — the route, the size of the notes, the system message, both questions,
  the context size of each arm and the summary itself.
- **Summary** — per arm: context messages, requests, input, cache read, cache
  write, output, cost and elapsed. A bar chart compares the input tokens.
- **Compaction** — the cold request's input reduction, the two-request cost
  reduction, and why a cached prefix can make the whole arm cheaper than its token
  count suggests.
- **Per request** — the arm, the step, the projection size, the messages shared
  with the previous request (and whether they are byte-identical), the shared
  digest, the provider's counters, the cost and the elapsed time.
- **Prefix reuse** — whether the warm request really re-sent the cold request's
  messages unchanged, which is the condition a cache hit needs.
- **Answers** — both replies, quoted per request. A request that committed no
  text says so; the benchmark does not grade the answers.
- **Reading** — what the saving does and does not include.

Each run also writes `<stem>.jsonl`, `<stem>-arms.csv`, `<stem>-requests.csv` and
a manifest beside the markdown.

## How to read it

- **The summary is written by the host, not by the model.** Norte has no
  compaction policy: dropping or summarizing history is a host decision. A summary
  produced by a model would be a request of its own, paid for at the transcript's
  size, so this run measures what compaction saves at request time, not what
  producing the summary costs.
- **Only money, tokens and time are measured.** Whether the compacted answer is as
  good is not a number this run produces, and the summary gives the compacted arm
  the answers. The two answers are quoted so a reader can see what happened, not
  as a quality score.
- **Cache reads change the conclusion.** A long, stable prefix can be served from
  the provider's cache, so the whole arm may cost less than its input tokens
  suggest. Compare the two rows and the `cache read` column before choosing a
  policy.
- **Two samples are not a latency benchmark.** The elapsed column is one
  measurement per request; do not read the difference between the arms as a
  latency result.
- **Cold means first in the arm, not globally cold.** The notes are deterministic,
  so a previous run with the same prefix can leave a cache that this run's first
  request reads. The `cache read` column is the evidence; a zero beside a
  byte-identical prefix means the provider did not serve one, not that the prefix
  changed.
- **The notes are deterministic.** The transcript is generated from the run's
  `--turns` and `--lines`, so two runs on the same route send the same bytes.

## Results

Canonical run: `results/2026-10-19T220225-baidu-deepseek-deepseek-v4-flash-0731.md`
(`deepseek/deepseek-v4-flash-0731` at Baidu, 2 s pause).

| arm | requests | input | cache read | output | cost |
| --- | ---: | ---: | ---: | ---: | ---: |
| whole | 2 | 30,256 | 14,336 | 1,403 | $0.002672 |
| compacted | 2 | 483 | 0 | 120 | $0.000109 |

Compaction cut request cost by 95.9% on this run; the cold request still read
14,336 tokens from a cache left by an earlier deterministic run, so the saving is
smaller than the 98.5% input reduction. The whole arm's cold request spent its
1,024-token output budget on reasoning and committed no text, which the report
shows in place of an answer. The two answers that were committed are quoted in
the report and are not graded.
