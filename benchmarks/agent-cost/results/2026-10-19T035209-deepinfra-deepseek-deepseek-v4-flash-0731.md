# agent-cost — `deepseek/deepseek-v4-flash-0731` @ DeepInfra

A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.

## The run

| | |
| --- | --- |
| when | 2026-10-19 03:52:09 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | DeepInfra |
| endpoint | `https://openrouter.ai/api/v1` |
| rustychain | 0.1.0 |
| commit | `eeee47d` |
| budget | 32 request(s) per task, 3 task(s) |
| scratch | `/tmp/rustychain-bench-agent-cost` |

## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=DeepInfra
cargo run -p rustychain-bench-agent-cost
```

## Tasks

| # | requests | input | cache read | hit | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 5 | 20594 | 17408 | 84.5 % | 825 | 0.000493 |
| 2 | 4 | 14152 | 12544 | 88.6 % | 1603 | 0.000808 |
| 3 | 4 | 14644 | 12544 | 85.7 % | 1980 | 0.000963 |
| **all** | 13 | 49390 | 42496 | 86.0 % | 4408 | 0.002263 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 8 call(s) — `command` ×2, `edit` ×3, `read` ×3
- **task 2**: 7 call(s) — `command` ×3 (3 failed), `read` ×4
- **task 3**: 7 call(s) — `command` ×1 (1 failed), `read` ×6

22 call(s) in all, 4 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.

## The bill

- the provider's `usage.cost`, summed over 13 of 13 request(s): **$0.002263**
- per task: $0.000754 · per request: $0.000174 · a hundred tasks: $0.0754
- cache hit: 86.0 % of the input (42496 of 49390 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.

## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `5 words: 2, the, sky,, is, blue.` and not `4 words: the, sky`

## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2658 | 2560 | 120 | 0.000071 |
| 1 | 2 | 3567 | 2560 | 463 | 0.000226 |
| 1 | 3 | 4211 | 3328 | 101 | 0.000066 |
| 1 | 4 | 4834 | 4096 | 111 | 0.000081 |
| 1 | 5 | 5324 | 4864 | 30 | 0.000048 |
| 2 | 1 | 2672 | 2560 | 121 | 0.000072 |
| 2 | 2 | 3586 | 2560 | 128 | 0.000075 |
| 2 | 3 | 3872 | 3584 | 330 | 0.000174 |
| 2 | 4 | 4022 | 3840 | 1024 | 0.000487 |
| 3 | 1 | 2667 | 2560 | 151 | 0.000081 |
| 3 | 2 | 3807 | 2560 | 61 | 0.000046 |
| 3 | 3 | 3972 | 3584 | 744 | 0.000354 |
| 3 | 4 | 4198 | 3840 | 1024 | 0.000481 |
