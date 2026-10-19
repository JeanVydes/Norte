# Transport retry under host policy
The transport never retries (FR-ERR-4): a `429`, a `5xx` or a dead connection reaches the host as a typed error, `Retry-After` intact and `is_transient` to say whether waiting could clear it. Every attempt below is the host deciding to make one. A failed turn stays `Streaming`, so the resume picks up from the committed head and re-sends the identical request (FR-ERR-5).


## Run

|  |  |
| --- | --- |
| mode | offline (scripted failures, no provider) |
| scenarios | 8 |
| rounds per scenario | 20 |
| simulated sleep | 1 ms per described wait |


## Scenarios

| scenario | injected | attempts | ok | modelled wait ms | request bytes | median µs | surfaced |
| --- | --- | ---: | --- | ---: | --- | ---: | --- |
| control |  | 1 | yes | 0 | identical | 988 |  |
| one-429-with-retry-after | RateLimited 429 retry-after 2000 | 2 | yes | 2000 | identical | 3779 |  |
| two-429s-second-silent | RateLimited 429 retry-after 300, RateLimited 429 | 3 | yes | 1300 | identical | 6833 |  |
| three-500s | ServerError 500, ServerError 502, ServerError 503 | 4 | yes | 3500 | identical | 9985 |  |
| overloaded-then-ok | ModelOverloaded 503 | 2 | yes | 500 | identical | 3925 |  |
| transport-timeout | Transport | 2 | yes | 500 | identical | 3812 |  |
| exhausts-max-attempts | RateLimited 429, RateLimited 429, RateLimited 429, RateLimited 429 | 3 | no | 1500 | identical | 6574 | Some(RateLimited): provider error: RateLimited: scripted failure (status 429) |
| permanent-401 | Authentication 401 | 1 | no | 0 | identical | 781 | Some(Authentication): provider error: Authentication: scripted failure (status 401) |

control  ██████░░░░░░░░░░░░░░░░░░  1
one-429-with-retry-after  ████████████░░░░░░░░░░░░  2
two-429s-second-silent  ██████████████████░░░░░░  3
three-500s  ████████████████████████  4
overloaded-then-ok  ████████████░░░░░░░░░░░░  2
transport-timeout  ████████████░░░░░░░░░░░░  2
exhausts-max-attempts  ██████████████████░░░░░░  3
permanent-401  ██████░░░░░░░░░░░░░░░░░░  1


## Traces

**control** — 1 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | ok | - | - | - | false | 0 |

**one-429-with-retry-after** — 2 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | RateLimited | 429 | 2000 | true | 2000 |
| 2 | ok | - | - | - | false | 0 |

**two-429s-second-silent** — 3 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | RateLimited | 429 | 300 | true | 300 |
| 2 | retry | RateLimited | 429 | - | true | 1000 |
| 3 | ok | - | - | - | false | 0 |

**three-500s** — 4 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | ServerError | 500 | - | true | 500 |
| 2 | retry | ServerError | 502 | - | true | 1000 |
| 3 | retry | ServerError | 503 | - | true | 2000 |
| 4 | ok | - | - | - | false | 0 |

**overloaded-then-ok** — 2 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | ModelOverloaded | 503 | - | true | 500 |
| 2 | ok | - | - | - | false | 0 |

**transport-timeout** — 2 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | Transport | - | - | true | 500 |
| 2 | ok | - | - | - | false | 0 |

**exhausts-max-attempts** — 3 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | RateLimited | 429 | - | true | 500 |
| 2 | retry | RateLimited | 429 | - | true | 1000 |
| 3 | retry | RateLimited | 429 | - | true | 0 |

**permanent-401** — 1 attempt(s)

| attempt | result | kind | status | retry-after ms | transient | waited ms |
| ---: | --- | --- | ---: | ---: | --- | ---: |
| 1 | retry | Authentication | 401 | - | false | 0 |


## What a retry costs

A retry is not free and it is not a new conversation. `request bytes: identical` means the resumed turn re-sent a request whose `sha256` did not move across attempts, so the provider's cache still applies and the host paid for time, not for a fresh prefix. Where the policy stops, the error is surfaced with its kind and status rather than flattened.

