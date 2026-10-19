# agent-cost — `deepseek/deepseek-v4-flash-0731` @ DeepInfra

A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.

## The run

| | |
| --- | --- |
| when | 2026-10-19 03:41:09 UTC |
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
| 1 | 16 | 69314 | 50688 | 73.1 % | 1954 | 0.001659 |
| 2 | 4 | 14247 | 9728 | 68.3 % | 1645 | 0.000833 |
| 3 | 16 | 105390 | 76288 | 72.4 % | 5783 | 0.003831 |
| **all** | 36 | 188951 | 136704 | 72.3 % | 9382 | 0.006323 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 18 call(s) — `command` ×15 (15 failed), `read` ×3
- **task 2**: 7 call(s) — `command` ×2 (2 failed), `read` ×5
- **task 3**: 22 call(s) — `command` ×6 (4 failed), `edit` ×10 (3 failed), `read` ×6

47 call(s) in all, 24 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.

## The bill

- the provider's `usage.cost`, summed over 36 of 36 request(s): **$0.006323**
- per task: $0.002108 · per request: $0.000176 · a hundred tasks: $0.2108
- cache hit: 72.3 % of the input (136704 of 188951 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.

## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `4 words: the, sky,` and not `4 words: the, sky`

## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2658 | 0 | 120 | 0.000071 |
| 1 | 2 | 3567 | 0 | 126 | 0.000141 |
| 1 | 3 | 3677 | 0 | 89 | 0.000124 |
| 1 | 4 | 3804 | 3328 | 86 | 0.000074 |
| 1 | 5 | 3950 | 3584 | 86 | 0.000073 |
| 1 | 6 | 4079 | 3584 | 89 | 0.000077 |
| 1 | 7 | 4203 | 0 | 132 | 0.000158 |
| 1 | 8 | 4302 | 3840 | 83 | 0.000075 |
| 1 | 9 | 4445 | 4096 | 563 | 0.000328 |
| 1 | 10 | 4567 | 4352 | 66 | 0.000064 |
| 1 | 11 | 4693 | 4608 | 62 | 0.000060 |
| 1 | 12 | 4815 | 4352 | 130 | 0.000103 |
| 1 | 13 | 4939 | 4608 | 64 | 0.000067 |
| 1 | 14 | 5063 | 4864 | 88 | 0.000078 |
| 1 | 15 | 5192 | 4608 | 108 | 0.000095 |
| 1 | 16 | 5360 | 4864 | 62 | 0.000070 |
| 2 | 1 | 2672 | 0 | 123 | 0.000073 |
| 2 | 2 | 3581 | 2560 | 213 | 0.000119 |
| 2 | 3 | 3895 | 3328 | 285 | 0.000154 |
| 2 | 4 | 4099 | 3840 | 1024 | 0.000487 |
| 3 | 1 | 2667 | 2560 | 148 | 0.000084 |
| 3 | 2 | 3802 | 2560 | 55 | 0.000049 |
| 3 | 3 | 3967 | 3584 | 820 | 0.000395 |
| 3 | 4 | 4538 | 3840 | 575 | 0.000288 |
| 3 | 5 | 5187 | 4352 | 992 | 0.000480 |
| 3 | 6 | 5486 | 5120 | 163 | 0.000109 |
| 3 | 7 | 6096 | 5376 | 92 | 0.000081 |
| 3 | 8 | 6241 | 0 | 129 | 0.000199 |
| 3 | 9 | 6430 | 0 | 128 | 0.000203 |
| 3 | 10 | 7265 | 6400 | 181 | 0.000150 |
| 3 | 11 | 7574 | 7168 | 731 | 0.000435 |
| 3 | 12 | 8208 | 0 | 258 | 0.000309 |
| 3 | 13 | 8741 | 7424 | 312 | 0.000234 |
| 3 | 14 | 9354 | 8960 | 349 | 0.000243 |
| 3 | 15 | 9730 | 9216 | 227 | 0.000182 |
| 3 | 16 | 10104 | 9728 | 623 | 0.000391 |
