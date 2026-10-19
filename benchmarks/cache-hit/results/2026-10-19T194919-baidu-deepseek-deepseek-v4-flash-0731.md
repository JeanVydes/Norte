# cache-hit
What survives a turn, a tool call, a park and an export, in the provider's own numbers.


## The run

|  |  |
| --- | --- |
| started | 2026-10-19 19:49:19 UTC |
| model | `deepseek/deepseek-v4-flash-0731` |
| provider | baidu |
| endpoint | `https://openrouter.ai/api/v1` |
| pause | 2000 ms before each request |
| phases | normal, tool, approval, export |


## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4-flash-0731
export BENCH_PROVIDER=baidu
export BENCH_PAUSE_MS=2000
cargo run -p norte-bench-cache-hit
```


## Turns

Every request waits `BENCH_PAUSE_MS` before it is sent, so the provider can publish the prefix cache of the request before it. The wait is included in the elapsed column and is protocol, not model time.

| phase | turn | elapsed |
| --- | --- | ---: |
| 1. normal | turn 1 (nothing to reuse yet) | 5.31 s |
| 1. normal | turn 2 (the conversation again) | 3.85 s |
| 2. tool | the turn that calls the tool | 7.10 s |
| 2. tool | the turn after the result | 3.63 s |
| 3. approval | the turn that parks | 3.61 s |
| 3. approval | the resume (not a new request: the parked one) | 3.34 s |
| 3. approval | the turn after the approval | 3.11 s |
| 4. export | the first turn of the new process | 4.19 s |


## Requests

| # | phase | turn | input | output | cache read | cache write | hit | cost |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1. normal | turn 1 (nothing to reuse yet) | 9199 | 151 | 0 | 0 | 0.0% | $0.001253 |
| 2 | 1. normal | turn 2 (the conversation again) | 9245 | 56 | 8192 | 0 | 88.6% | $0.000192 |
| 3 | 2. tool | the turn that calls the tool | 9296 | 75 | 9216 | 0 | 99.1% | $0.000078 |
| 4 | 2. tool | the turn that calls the tool | 9359 | 37 | 9216 | 0 | 98.5% | $0.000071 |
| 5 | 2. tool | the turn after the result | 9371 | 37 | 9216 | 0 | 98.3% | $0.000073 |
| 6 | 3. approval | the turn that parks | 9390 | 63 | 9216 | 0 | 98.1% | $0.000085 |
| 7 | 3. approval | the resume (not a new request: the parked one) | 9477 | 21 | 9216 | 0 | 97.2% | $0.000080 |
| 8 | 3. approval | the turn after the approval | 9507 | 39 | 9216 | 0 | 96.9% | $0.000091 |
| 9 | 4. export | the first turn of the new process | 9538 | 94 | 9472 | 0 | 99.3% | $0.000084 |

**Bill.** $0.002007 across 9 request(s).


## Prefix audit

| phase | request | messages | bytes | shared | status | digest |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1. normal | request 1 | 2 | 39843 | — | — | — |
| 1. normal | request 2 | 4 | 40114 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 1 | 2 | 39843 | — | — | — |
| 2. tool | request 2 | 4 | 40114 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 3 | 6 | 40395 | 4 | byte-identical | f3737558d43b854213ab443913c427e5b942d155b01578c7e24009707515a979 |
| 2. tool | request 4 | 8 | 40674 | 6 | byte-identical | d03f0f81698ad316954002b6bfbd9e4beca169dbc894146b1dadbfac7cb74b1b |
| 2. tool | request 5 | 10 | 40757 | 8 | byte-identical | 10690ebf715e5d009e39d2895f5d05131f6cd375430024ab8dbf64416c337259 |
| 3. approval | request 1 | 2 | 39843 | — | — | — |
| 3. approval | request 2 | 4 | 40114 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 3. approval | request 3 | 6 | 40395 | 4 | byte-identical | f3737558d43b854213ab443913c427e5b942d155b01578c7e24009707515a979 |
| 3. approval | request 4 | 8 | 40674 | 6 | byte-identical | d03f0f81698ad316954002b6bfbd9e4beca169dbc894146b1dadbfac7cb74b1b |
| 3. approval | request 5 | 10 | 40757 | 8 | byte-identical | 10690ebf715e5d009e39d2895f5d05131f6cd375430024ab8dbf64416c337259 |
| 3. approval | request 6 | 12 | 40876 | 10 | byte-identical | ac0a0c1a5620fe0671b1c7b1ed7e93ee5abde8701837326fc87f5ab6f3e29b9c |
| 3. approval | request 7 | 14 | 41209 | 12 | byte-identical | 24aa622a80b8120b441ed222632dae0dc5549b52191fc84ac7c73999bfb472eb |
| 3. approval | request 8 | 16 | 41390 | 14 | byte-identical | fe5f84b254f608033a26de0a0f7db6bd4e8b3643b09d963a99d6a45ca828df3a |
| 4. export | the other engine's last request | 16 | 41390 | — | — | — |
| 4. export | request 1 | 18 | 41557 | 16 | byte-identical | e413cbe69ef07021790b475aff2730a84021f3c27b2ee7c041a16a6337ea0789 |


## What this shows

the provider reported 72960 cached token(s) across 8 request(s), each one on a request whose shared prefix the audit above shows byte-identical to the request before it — including the resume after the approval and the first turn after the import.

