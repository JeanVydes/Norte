# cache-hit
What survives a turn, a tool call, a park and an export, in the provider's own numbers.


## The run

|  |  |
| --- | --- |
| started | 2026-10-19 19:26:40 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | baidu |
| endpoint | `https://openrouter.ai/api/v1` |
| phases | normal, tool, approval, export |


## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=baidu
cargo run -p norte-bench-cache-hit
```


## Turns

| phase | turn | elapsed |
| --- | --- | ---: |
| 1. normal | turn 1 (nothing to reuse yet) | 2.78 s |
| 1. normal | turn 2 (the conversation again) | 1.47 s |
| 2. tool | the turn that calls the tool | 4.01 s |
| 2. tool | the turn after the result | 1.21 s |
| 3. approval | the turn that parks | 1.46 s |
| 3. approval | the resume (not a new request: the parked one) | 1.44 s |
| 3. approval | the turn after the approval | 1.65 s |
| 4. export | the first turn of the new process | 1.90 s |


## Requests

| # | phase | turn | input | output | cache read | cache write | hit | cost |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1. normal | turn 1 (nothing to reuse yet) | 9199 | 31 | 0 | 0 | 0.0% | $0.001206 |
| 2 | 1. normal | turn 2 (the conversation again) | 9244 | 62 | 0 | 0 | 0.0% | $0.001224 |
| 3 | 2. tool | the turn that calls the tool | 9291 | 72 | 0 | 0 | 0.0% | $0.001234 |
| 4 | 2. tool | the turn that calls the tool | 9354 | 16 | 9216 | 0 | 98.5% | $0.000062 |
| 5 | 2. tool | the turn after the result | 9378 | 38 | 9216 | 0 | 98.3% | $0.000074 |
| 6 | 3. approval | the turn that parks | 9397 | 63 | 9216 | 0 | 98.1% | $0.000086 |
| 7 | 3. approval | the resume (not a new request: the parked one) | 9484 | 20 | 9216 | 0 | 97.2% | $0.000081 |
| 8 | 3. approval | the turn after the approval | 9513 | 17 | 9216 | 0 | 96.9% | $0.000083 |
| 9 | 4. export | the first turn of the new process | 9544 | 46 | 9472 | 0 | 99.2% | $0.000066 |

**Bill.** $0.004117 across 9 request(s).


## Prefix audit

| phase | request | messages | bytes | shared | status | digest |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1. normal | request 1 | 2 | 39843 | — | — | — |
| 1. normal | request 2 | 4 | 40142 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 1 | 2 | 39843 | — | — | — |
| 2. tool | request 2 | 4 | 40142 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 3 | 6 | 40408 | 4 | byte-identical | 6a0097776859a92cf4fee8c1f0cf1fdcc819d43d2a55bbec642bfc2f31fa4450 |
| 2. tool | request 4 | 8 | 40687 | 6 | byte-identical | 8b05153084ec1b672e4a0f6a84e51cd0bc2301f0a51829986ad17ac1868c4776 |
| 2. tool | request 5 | 10 | 40824 | 8 | byte-identical | 51140c0834a53f014b590be9d61f39a59858030eae99e62cbdfbd44542e21dda |
| 3. approval | request 1 | 2 | 39843 | — | — | — |
| 3. approval | request 2 | 4 | 40142 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 3. approval | request 3 | 6 | 40408 | 4 | byte-identical | 6a0097776859a92cf4fee8c1f0cf1fdcc819d43d2a55bbec642bfc2f31fa4450 |
| 3. approval | request 4 | 8 | 40687 | 6 | byte-identical | 8b05153084ec1b672e4a0f6a84e51cd0bc2301f0a51829986ad17ac1868c4776 |
| 3. approval | request 5 | 10 | 40824 | 8 | byte-identical | 51140c0834a53f014b590be9d61f39a59858030eae99e62cbdfbd44542e21dda |
| 3. approval | request 6 | 12 | 40943 | 10 | byte-identical | f3105567f12571c87f925d2a91065027a59e7a37c5d55f7815d61a03914e3a46 |
| 3. approval | request 7 | 14 | 41276 | 12 | byte-identical | 58b41504f1f0182fc7bfe34b33b5a2f40c566f22c296a9ec9a4f6d21f8047d8d |
| 3. approval | request 8 | 16 | 41448 | 14 | byte-identical | 677c0aeea09e32363f5c0f8063743cd371ef976aa95cb00ba656c283da590baf |
| 4. export | the other engine's last request | 16 | 41448 | — | — | — |
| 4. export | request 1 | 18 | 41615 | 16 | byte-identical | e64024f8d356f048408d71b295cabeddd483e5a1dba33d3a5618bed3eb7e501a |


## What this shows

the provider reported 55552 cached token(s) across 6 request(s), each one on a request whose shared prefix the audit above shows byte-identical to the request before it — including the resume after the approval and the first turn after the import.

