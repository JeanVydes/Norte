# agent-cost — `deepseek/deepseek-v4-flash-0731` @ DeepInfra

A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.

## The run

| | |
| --- | --- |
| when | 2026-10-19 05:17:05 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | DeepInfra |
| endpoint | `https://openrouter.ai/api/v1` |
| norte | 0.0.1 |
| commit | `3b1aae8` |
| budget | 32 request(s) per task, 3 task(s) |
| scratch | `/tmp/norte-bench-agent-cost` |

## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=DeepInfra
cargo run -p norte-bench-agent-cost
```

## Tasks

| # | requests | input | cache read | hit | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 8 | 33796 | 27392 | 81.1 % | 1207 | 0.001012 |
| 2 | 8 | 34087 | 29696 | 87.1 % | 1454 | 0.000971 |
| 3 | 2 | 6002 | 4864 | 81.0 % | 210 | 0.000179 |
| **all** | 18 | 73885 | 61952 | 83.8 % | 2871 | 0.002162 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 11 call(s) — `command` ×5 (4 failed), `edit` ×3, `read` ×3
- **task 2**: 10 call(s) — `command` ×4 (2 failed), `edit` ×2, `read` ×4
- **task 3**: 5 call(s) — `read` ×5

26 call(s) in all, 6 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.

## The bill

- the provider's `usage.cost`, summed over 18 of 18 request(s): **$0.002162**
- per task: $0.000721 · per request: $0.000120 · a hundred tasks: $0.0721
- cache hit: 83.8 % of the input (61952 of 73885 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.

## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `5 words: 2, the, sky, is, blue` and not `4 words: the, sky`

## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2642 | 0 | 116 | 0.000179 |
| 1 | 2 | 3545 | 2560 | 459 | 0.000180 |
| 1 | 3 | 4179 | 3328 | 73 | 0.000114 |
| 1 | 4 | 4312 | 4096 | 91 | 0.000091 |
| 1 | 5 | 4463 | 4096 | 125 | 0.000106 |
| 1 | 6 | 4630 | 4352 | 156 | 0.000110 |
| 1 | 7 | 4780 | 4352 | 163 | 0.000120 |
| 1 | 8 | 5245 | 4608 | 24 | 0.000112 |
| 2 | 1 | 2656 | 2304 | 120 | 0.000077 |
| 2 | 2 | 3564 | 2560 | 117 | 0.000120 |
| 2 | 3 | 3839 | 3328 | 666 | 0.000200 |
| 2 | 4 | 4300 | 3584 | 189 | 0.000131 |
| 2 | 5 | 4542 | 4096 | 124 | 0.000111 |
| 2 | 6 | 4682 | 4352 | 90 | 0.000101 |
| 2 | 7 | 5148 | 4608 | 134 | 0.000126 |
| 2 | 8 | 5356 | 4864 | 14 | 0.000105 |
| 3 | 1 | 2651 | 2304 | 91 | 0.000072 |
| 3 | 2 | 3351 | 2560 | 119 | 0.000107 |
