# agent-cost — `deepseek/deepseek-v4-flash-0731` @ DeepInfra

A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.

## The run

| | |
| --- | --- |
| when | 2026-10-19 05:18:31 UTC |
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
| 1 | 12 | 54850 | 50176 | 91.5 % | 2115 | 0.001414 |
| 2 | 11 | 51747 | 41216 | 79.6 % | 2276 | 0.001660 |
| 3 | 16 | 98021 | 87040 | 88.8 % | 3695 | 0.002630 |
| **all** | 39 | 204618 | 178432 | 87.2 % | 8086 | 0.005703 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 16 call(s) — `command` ×10 (8 failed), `edit` ×3, `read` ×3
- **task 2**: 15 call(s) — `command` ×7 (5 failed), `edit` ×4, `read` ×4
- **task 3**: 18 call(s) — `command` ×4 (3 failed), `edit` ×7 (1 failed), `read` ×7

49 call(s) in all, 17 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.

## The bill

- the provider's `usage.cost`, summed over 39 of 39 request(s): **$0.005703**
- per task: $0.001901 · per request: $0.000146 · a hundred tasks: $0.1901
- cache hit: 87.2 % of the input (178432 of 204618 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.

## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `4 words: the, sky`

## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2642 | 2560 | 115 | 0.000064 |
| 1 | 2 | 3545 | 3328 | 96 | 0.000080 |
| 1 | 3 | 3676 | 3328 | 151 | 0.000098 |
| 1 | 4 | 3818 | 3584 | 128 | 0.000091 |
| 1 | 5 | 3998 | 3584 | 476 | 0.000164 |
| 1 | 6 | 4180 | 3840 | 568 | 0.000180 |
| 1 | 7 | 4814 | 4096 | 120 | 0.000126 |
| 1 | 8 | 4994 | 4608 | 216 | 0.000131 |
| 1 | 9 | 5311 | 4864 | 79 | 0.000114 |
| 1 | 10 | 5752 | 5120 | 67 | 0.000127 |
| 1 | 11 | 5849 | 5632 | 66 | 0.000109 |
| 1 | 12 | 6271 | 5632 | 33 | 0.000129 |
| 2 | 1 | 2656 | 2560 | 116 | 0.000065 |
| 2 | 2 | 3564 | 3072 | 46 | 0.000084 |
| 2 | 3 | 3727 | 0 | 595 | 0.000331 |
| 2 | 4 | 4215 | 3584 | 180 | 0.000124 |
| 2 | 5 | 4448 | 4096 | 285 | 0.000134 |
| 2 | 6 | 4787 | 4352 | 213 | 0.000130 |
| 2 | 7 | 5020 | 4608 | 174 | 0.000125 |
| 2 | 8 | 5311 | 4864 | 272 | 0.000149 |
| 2 | 9 | 5700 | 5120 | 114 | 0.000132 |
| 2 | 10 | 5817 | 3328 | 269 | 0.000248 |
| 2 | 11 | 6502 | 5632 | 12 | 0.000139 |
| 3 | 1 | 2651 | 256 | 88 | 0.000163 |
| 3 | 2 | 3192 | 2560 | 92 | 0.000093 |
| 3 | 3 | 3969 | 3072 | 54 | 0.000110 |
| 3 | 4 | 4132 | 3840 | 453 | 0.000157 |
| 3 | 5 | 4620 | 3840 | 727 | 0.000235 |
| 3 | 6 | 5226 | 4352 | 127 | 0.000141 |
| 3 | 7 | 5617 | 5120 | 712 | 0.000235 |
| 3 | 8 | 6247 | 5376 | 46 | 0.000141 |
| 3 | 9 | 6637 | 6144 | 286 | 0.000173 |
| 3 | 10 | 7003 | 6400 | 341 | 0.000194 |
| 3 | 11 | 7365 | 6912 | 224 | 0.000171 |
| 3 | 12 | 7889 | 7168 | 284 | 0.000202 |
| 3 | 13 | 8194 | 7680 | 63 | 0.000157 |
| 3 | 14 | 8317 | 7936 | 49 | 0.000151 |
| 3 | 15 | 8426 | 8192 | 66 | 0.000149 |
| 3 | 16 | 8536 | 8192 | 83 | 0.000158 |
