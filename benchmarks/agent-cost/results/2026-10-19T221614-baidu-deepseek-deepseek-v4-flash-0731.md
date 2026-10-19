# agent-cost — `deepseek/deepseek-v4-flash-0731` @ baidu
A coding agent on a crate it did not write: rename a public function, change what a word is and fix the test that pinned the old rule, thread a flag from the command line. Nothing is written from scratch, so the agent has to read before it edits, and it can build the crate with the `command` tool — a compiler error is a tool result it reads and fixes inside the turn.

Every figure is the provider's own: the `usage.cost` each response reported, summed, and the token counters it reported. No rate is applied here, so a request whose payload carried no `cost` is counted and named as such rather than priced by this file.


## The run

|  |  |
| --- | --- |
| when | 2026-10-19 22:16:14 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | baidu |
| endpoint | `https://openrouter.ai/api/v1` |
| pause | 2000 ms before each request |
| norte | 0.0.2 |
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
| 1 | 3 | 9937 | 8448 | 85.0 % | 352 | 0.000365 |
| 2 | 16 | 73306 | 65792 | 89.7 % | 2556 | 0.002242 |
| 3 | 12 | 60653 | 51968 | 85.7 % | 2242 | 0.002215 |
| all | 31 | 143896 | 126208 | 87.7 % | 5150 | 0.004823 |

### What the tasks asked

1. Rename the public function `head_words` to `words_up_to` everywhere: its definition in `src/lib.rs`, the call in `report`, the import and use in `tests/words.rs`, and the API list in `README.md`. Nothing in the crate may refer to the old name. Answer with the files you changed.
2. A word currently keeps the punctuation it was written with, so `--verbose "the sky, is blue."` prints `sky,` and `blue.`. Make a word carry no leading or trailing punctuation, in both the count and the list, and fix the test in `tests/words.rs` that pinned the old rule — its name is wrong now too. Answer with the report that line prints.
3. Add a `--limit N` flag to `src/main.rs`: `Config` already carries a `limit` field that nothing sets from the command line, so `--verbose --limit 2` should report only the first two words. The `2` is the flag's value, not a word, so it must not reach the text. Answer with the code you added.

## Tool calls

- **task 1**: 5 call(s) — `command` ×2 (2 failed), `read` ×3
- **task 2**: 20 call(s) — `command` ×16 (16 failed), `read` ×4
- **task 3**: 16 call(s) — `command` ×7 (4 failed), `edit` ×4, `read` ×5

41 call(s) in all, 22 refused or failed. A call that fails is billed like any other request, because the round that carried it was paid for before the tool ever ran, which is why it is counted here.

Failed calls, by tool, reason and text:
- `command` ×22 (schema): command: the request needs `command` as an object with `kind` and `program`, for example {"command":{"kind":"program","program":"cargo","args":["test","--offline"]}}; the shell is not available


## The bill

- the provider's `usage.cost`, summed over 31 of 31 request(s): **$0.004823**
- per task: $0.001608 · per request: $0.000156 · a hundred tasks: $0.1608
- cache hit: 87.7 % of the input (126208 of 143896 tokens). A read is counted inside `input` on this wire, so it is already part of the total above; what the hit was worth is in that number, not in a rate applied here.


## The host's check

- **cargo test**: the crate builds and every test passes
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
  - `test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s`
- **cargo run -- --verbose --limit 2 "the sky, is blue."**: prints `4 words: the, sky,` and not `4 words: the, sky`


## Requests

Every model request the engine made, in order, with the usage the provider reported for each. A turn's outcome hides these: the transcript is re-sent and re-billed once per iteration, which is what an agent's bill is made of.

| task | request | input | cache read | output | cost (USD) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1 | 2661 | 2048 | 120 | 0.000135 |
| 1 | 2 | 3564 | 3072 | 135 | 0.000129 |
| 1 | 3 | 3712 | 3328 | 97 | 0.000101 |
| 2 | 1 | 2675 | 2048 | 151 | 0.000149 |
| 2 | 2 | 3666 | 2048 | 105 | 0.000259 |
| 2 | 3 | 3917 | 3584 | 96 | 0.000095 |
| 2 | 4 | 4041 | 3840 | 92 | 0.000078 |
| 2 | 5 | 4174 | 3072 | 83 | 0.000188 |
| 2 | 6 | 4317 | 4096 | 101 | 0.000085 |
| 2 | 7 | 4460 | 4096 | 122 | 0.000112 |
| 2 | 8 | 4606 | 4096 | 235 | 0.000175 |
| 2 | 9 | 4744 | 4352 | 164 | 0.000133 |
| 2 | 10 | 4866 | 4352 | 188 | 0.000158 |
| 2 | 11 | 4999 | 4864 | 174 | 0.000105 |
| 2 | 12 | 5119 | 4864 | 155 | 0.000114 |
| 2 | 13 | 5243 | 4864 | 204 | 0.000149 |
| 2 | 14 | 5365 | 5120 | 94 | 0.000090 |
| 2 | 15 | 5482 | 5120 | 179 | 0.000138 |
| 2 | 16 | 5632 | 5376 | 413 | 0.000216 |
| 3 | 1 | 2670 | 2560 | 151 | 0.000084 |
| 3 | 2 | 3797 | 2048 | 75 | 0.000265 |
| 3 | 3 | 3960 | 3072 | 680 | 0.000393 |
| 3 | 4 | 4653 | 3072 | 376 | 0.000364 |
| 3 | 5 | 4964 | 4096 | 81 | 0.000161 |
| 3 | 6 | 5105 | 4864 | 80 | 0.000083 |
| 3 | 7 | 5245 | 4864 | 66 | 0.000095 |
| 3 | 8 | 5371 | 5120 | 82 | 0.000086 |
| 3 | 9 | 5513 | 5120 | 122 | 0.000120 |
| 3 | 10 | 6143 | 5120 | 122 | 0.000201 |
| 3 | 11 | 6305 | 5888 | 136 | 0.000131 |
| 3 | 12 | 6927 | 6144 | 271 | 0.000233 |

