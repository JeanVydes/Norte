# cache-hit
What survives a turn, a tool call, a park and an export, in the provider's own numbers.


## The run

|  |  |
| --- | --- |
| started | 2026-10-19 20:09:39 UTC |
| model | `deepseek/deepseek-v4.1-flash-1017` |
| provider | deepinfra/fp8/flex |
| endpoint | `https://openrouter.ai/api/v1` |
| pause | 2000 ms before each request |
| phases | normal, tool, approval, export |


## Reproduce

```sh
export OPENROUTER_API_KEY=...
export BENCH_ENDPOINT=https://openrouter.ai/api/v1
export BENCH_MODEL=deepseek/deepseek-v4.1-flash-1017
export BENCH_PROVIDER=deepinfra/fp8/flex
export BENCH_PAUSE_MS=2000
cargo run -p norte-bench-cache-hit
```


## Turns

Every request waits `BENCH_PAUSE_MS` before it is sent, so the provider can publish the prefix cache of the request before it. The wait is included in the elapsed column and is protocol, not model time.

| phase | turn | elapsed |
| --- | --- | ---: |
| 1. normal | turn 1 (nothing to reuse yet) | 5.56 s |
| 1. normal | turn 2 (the conversation again) | 6.50 s |
| 2. tool | the turn that calls the tool | 8.77 s |
| 2. tool | the turn after the result | 8.00 s |
| 3. approval | the turn that parks | 3.53 s |
| 3. approval | the resume (not a new request: the parked one) | 8.66 s |
| 3. approval | the turn after the approval | 5.58 s |
| 4. export | the first turn of the new process | 5.11 s |


## Requests

| # | phase | turn | input | output | cache read | cache write | hit | cost |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1. normal | turn 1 (nothing to reuse yet) | 9136 | 44 | 0 | 0 | 0.0% | $0.000727 |
| 2 | 1. normal | turn 2 (the conversation again) | 9185 | 44 | 9088 | 0 | 98.9% | $0.000039 |
| 3 | 2. tool | the turn that calls the tool | 9238 | 52 | 9088 | 0 | 98.4% | $0.000045 |
| 4 | 2. tool | the turn that calls the tool | 9295 | 13 | 9216 | 0 | 99.2% | $0.000031 |
| 5 | 2. tool | the turn after the result | 9316 | 32 | 9216 | 0 | 98.9% | $0.000037 |
| 6 | 3. approval | the turn that parks | 9335 | 57 | 9216 | 0 | 98.7% | $0.000044 |
| 7 | 3. approval | the resume (not a new request: the parked one) | 9416 | 19 | 9344 | 0 | 99.2% | $0.000032 |
| 8 | 3. approval | the turn after the approval | 9444 | 29 | 9344 | 0 | 98.9% | $0.000037 |
| 9 | 4. export | the first turn of the new process | 9474 | 37 | 9344 | 0 | 98.6% | $0.000041 |

**Bill.** $0.001033 across 9 request(s).


## Prefix audit

| phase | request | messages | bytes | shared | status | digest |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1. normal | request 1 | 2 | 39857 | — | — | — |
| 1. normal | request 2 | 4 | 40133 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 1 | 2 | 39857 | — | — | — |
| 2. tool | request 2 | 4 | 40133 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 3 | 6 | 40414 | 4 | byte-identical | 9b542013b4de6c19ead77f98d1a02259322608db0d72fcd3e0d8ec94b43f9b58 |
| 2. tool | request 4 | 8 | 40694 | 6 | byte-identical | d3832688c7a12a49f2e363b4357e79264c0c8c4d21a4b13fd78a3b88f34b55a6 |
| 2. tool | request 5 | 10 | 40815 | 8 | byte-identical | cd8a2b7c445bcf31e1f0f9c9e5d7cc7f3e600f57015c4243f6ac0949245a6c3d |
| 3. approval | request 1 | 2 | 39857 | — | — | — |
| 3. approval | request 2 | 4 | 40133 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 3. approval | request 3 | 6 | 40414 | 4 | byte-identical | 9b542013b4de6c19ead77f98d1a02259322608db0d72fcd3e0d8ec94b43f9b58 |
| 3. approval | request 4 | 8 | 40694 | 6 | byte-identical | d3832688c7a12a49f2e363b4357e79264c0c8c4d21a4b13fd78a3b88f34b55a6 |
| 3. approval | request 5 | 10 | 40815 | 8 | byte-identical | cd8a2b7c445bcf31e1f0f9c9e5d7cc7f3e600f57015c4243f6ac0949245a6c3d |
| 3. approval | request 6 | 12 | 40934 | 10 | byte-identical | 05d82ae711b8ef2e10af3face02bdc3f29d210c08a0079d0c02c660c209e17fe |
| 3. approval | request 7 | 14 | 41269 | 12 | byte-identical | 09a515eeeeddd0cd35e44367fa54fec4ef490e710f8e97e58964c39f601310ba |
| 3. approval | request 8 | 16 | 41432 | 14 | byte-identical | 36bca25ff8f23403742e0bcd624bbecab082bdd2e54178c670c2c7eaf102afba |
| 4. export | the other engine's last request | 16 | 41432 | — | — | — |
| 4. export | request 1 | 18 | 41591 | 16 | byte-identical | a9644d333fc7d91d8c0620602cc89ec3e594933d3fcdd1c678e803a52e787d46 |


## What this shows

the provider reported 73856 cached token(s) across 8 request(s), each one on a request whose shared prefix the audit above shows byte-identical to the request before it — including the resume after the approval and the first turn after the import.

