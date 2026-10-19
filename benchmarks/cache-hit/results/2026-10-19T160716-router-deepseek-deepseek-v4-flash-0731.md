# cache-hit

What survives a turn, a tool call, a park and an export, in the provider's own numbers.

## The run

| | |
| --- | --- |
| Started | 2026-10-19 16:07:16 UTC |
| Model | `deepseek/deepseek-v4-flash-0731` |
| Provider | the router decides |
| Endpoint | `https://openrouter.ai/api/v1` |
| Commit | 5e2b731 |

## Reproduce

```sh
OPENROUTER_API_KEY=...
BENCH_ENDPOINT=https://openrouter.ai/api/v1
BENCH_MODEL=deepseek/deepseek-v4-flash-0731
cargo run -p norte-bench-cache-hit
```

## Turns

| Phase | Turn | Elapsed |
| --- | --- | ---: |
| 1. normal | turn 1 (nothing to reuse yet) | 5.96 s |
| 1. normal | turn 2 (the conversation again) | 4.13 s |
| 2. tool | the turn that calls the tool | 14.03 s |
| 2. tool | the turn after the result | 15.80 s |
| 3. approval | the turn that parks | 4.50 s |
| 3. approval | the resume (not a new request: the parked one) | 15.10 s |
| 3. approval | the turn after the approval | 49.98 s |
| 4. export | the first turn of the new process | 42.39 s |

## Requests

| # | Phase | Turn | Input | Output | Cache read | Cache write | Hit | Cost |
| ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 1. normal | turn 1 (nothing to reuse yet) | 9199 | 50 | 0 | 0 | 0.0% | $0.000220 |
| 2 | 1. normal | turn 2 (the conversation again) | 9241 | 50 | 0 | 0 | 0.0% | $0.000220 |
| 3 | 2. tool | the turn that calls the tool | 9290 | 93 | 256 | 0 | 2.8% | $0.001103 |
| 4 | 2. tool | the turn that calls the tool | 9353 | 43 | 9216 | 0 | 98.5% | $0.000246 |
| 5 | 2. tool | the turn after the result | 9388 | 512 | 9216 | 0 | 98.2% | $0.000362 |
| 6 | 3. approval | the turn that parks | 9872 | 128 | 9216 | 0 | 93.4% | $0.000328 |
| 7 | 3. approval | the resume (not a new request: the parked one) | 9959 | 512 | 9728 | 0 | 97.7% | $0.000381 |
| 8 | 3. approval | the turn after the approval | 10331 | 622 | 0 | 0 | 0.0% | $0.000146 |
| 9 | 4. export | the first turn of the new process | 10347 | 315 | 0 | 0 | 0.0% | $0.000109 |

**Bill.** $0.003114 across 9 request(s).

## Prefix audit

| Phase | Request | Messages | Bytes | Shared | Status | Digest |
| --- | --- | ---: | ---: | ---: | --- | --- |
| 1. normal | request 1 | 2 | 39788 | — | — | — |
| 1. normal | request 2 | 4 | 40033 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 1 | 2 | 39788 | — | — | — |
| 2. tool | request 2 | 4 | 40033 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 2. tool | request 3 | 6 | 40299 | 4 | byte-identical | e2164bcb229788edc964ddf4ae677c45369d590fab5032105b130056e6b504b1 |
| 2. tool | request 4 | 8 | 40579 | 6 | byte-identical | 164b1001c73b6e45e8aa2c8d707043af02d1e7d28ea1eba554122982fa22191d |
| 2. tool | request 5 | 10 | 40758 | 8 | byte-identical | 1691412d88c9f1bce58f6fb9dcaa90b0e61fe8dcafb77152fb7d01e006ce7447 |
| 3. approval | request 1 | 2 | 39788 | — | — | — |
| 3. approval | request 2 | 4 | 40033 | 2 | byte-identical | b7219ecf85ec995ffc7789571d2a5aa301112f0042763ba1078011de9847ac99 |
| 3. approval | request 3 | 6 | 40299 | 4 | byte-identical | e2164bcb229788edc964ddf4ae677c45369d590fab5032105b130056e6b504b1 |
| 3. approval | request 4 | 8 | 40579 | 6 | byte-identical | 164b1001c73b6e45e8aa2c8d707043af02d1e7d28ea1eba554122982fa22191d |
| 3. approval | request 5 | 10 | 40758 | 8 | byte-identical | 1691412d88c9f1bce58f6fb9dcaa90b0e61fe8dcafb77152fb7d01e006ce7447 |
| 3. approval | request 6 | 12 | 42265 | 10 | byte-identical | 16232e3b7e20cd74737a4494bc153a841cbafa1eff7228e8af91e0feb88a731d |
| 3. approval | request 7 | 14 | 42600 | 12 | byte-identical | a0990e512fc5b69425bb51c112bd2c013fa80c628a47ca2f81727b4aca5f961b |
| 3. approval | request 8 | 16 | 44328 | 14 | byte-identical | a59853541a8d40fb42f394a7a62bfd785386878ed5deabe13eb55a449ba64ac9 |
| 4. export | the other engine's last request | 16 | 44328 | — | — | — |
| 4. export | request 1 | 18 | 44429 | 16 | byte-identical | df35f0a7f053f0789967198baa148304f8580a5a096e69f5c26081714f687839 |

## What this shows

the provider reported 37632 cached token(s) across 5 request(s), each one on a request whose shared prefix the audit above shows byte-identical to the request before it — including the resume after the approval and the first turn after the import.

