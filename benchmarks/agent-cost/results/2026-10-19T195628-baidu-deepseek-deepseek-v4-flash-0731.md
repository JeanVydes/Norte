# agent-cost — `deepseek/deepseek-v4-flash-0731` @ baidu
A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.


## The run

|  |  |
| --- | --- |
| when | 2026-10-19 19:56:28 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | baidu |
| endpoint | `https://openrouter.ai/api/v1` |
| pause | 2000 ms before each request |
| norte | 0.0.1 |
| budget | 32 request(s) per task, 3 task(s) |
| scratch | `/tmp/norte-bench-agent-cost` |


## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=baidu
export BENCH_PAUSE_MS=2000
cargo run -p norte-bench-agent-cost
```


## Tasks

| # | requests | input | cache read | hit | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 9 | 39395 | 32256 | 81.9 % | 1706 | 0.001724 |
| 2 | 10 | 44473 | 38400 | 86.3 % | 2033 | 0.001739 |
| 3 | 13 | 65717 | 56320 | 85.7 % | 2488 | 0.002421 |
| all | 32 | 149585 | 126976 | 84.9 % | 6227 | 0.005884 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 13 call(s) — `command` ×5 (4 failed), `edit` ×3, `read` ×5
- **task 2**: 13 call(s) — `command` ×6 (4 failed), `edit` ×2, `read` ×5
- **task 3**: 15 call(s) — `command` ×8 (6 failed), `edit` ×2, `read` ×5

41 call(s) in all, 14 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.


## The bill

- the provider's `usage.cost`, summed over 32 of 32 request(s): **$0.005884**
- per task: $0.001961 · per request: $0.000184 · a hundred tasks: $0.1961
- cache hit: 84.9 % of the input (126976 of 149585 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.


## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `4 words: the, sky`


## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2661 | 0 | 108 | 0.000387 |
| 1 | 2 | 3564 | 2048 | 465 | 0.000386 |
| 1 | 3 | 4198 | 3328 | 73 | 0.000155 |
| 1 | 4 | 4331 | 4096 | 107 | 0.000089 |
| 1 | 5 | 4498 | 4096 | 66 | 0.000095 |
| 1 | 6 | 4624 | 4352 | 95 | 0.000090 |
| 1 | 7 | 4734 | 4608 | 602 | 0.000270 |
| 1 | 8 | 5199 | 4608 | 110 | 0.000139 |
| 1 | 9 | 5586 | 5120 | 80 | 0.000113 |
| 2 | 1 | 2675 | 2560 | 121 | 0.000073 |
| 2 | 2 | 3583 | 2560 | 123 | 0.000191 |
| 2 | 3 | 3856 | 3072 | 104 | 0.000155 |
| 2 | 4 | 4006 | 3072 | 146 | 0.000191 |
| 2 | 5 | 4130 | 3840 | 589 | 0.000283 |
| 2 | 6 | 4372 | 4096 | 654 | 0.000307 |
| 2 | 7 | 5064 | 4096 | 91 | 0.000178 |
| 2 | 8 | 5215 | 4864 | 90 | 0.000101 |
| 2 | 9 | 5682 | 5120 | 103 | 0.000134 |
| 2 | 10 | 5890 | 5120 | 12 | 0.000126 |
| 3 | 1 | 2670 | 2048 | 151 | 0.000148 |
| 3 | 2 | 3893 | 2048 | 57 | 0.000270 |
| 3 | 3 | 4056 | 3840 | 482 | 0.000232 |
| 3 | 4 | 4513 | 3072 | 181 | 0.000270 |
| 3 | 5 | 4745 | 4352 | 209 | 0.000150 |
| 3 | 6 | 4899 | 4096 | 86 | 0.000155 |
| 3 | 7 | 5045 | 4608 | 81 | 0.000107 |
| 3 | 8 | 5186 | 4864 | 148 | 0.000120 |
| 3 | 9 | 5813 | 5120 | 120 | 0.000158 |
| 3 | 10 | 5976 | 5120 | 360 | 0.000272 |
| 3 | 11 | 6138 | 5888 | 237 | 0.000149 |
| 3 | 12 | 6288 | 5120 | 141 | 0.000228 |
| 3 | 13 | 6495 | 6144 | 235 | 0.000162 |

