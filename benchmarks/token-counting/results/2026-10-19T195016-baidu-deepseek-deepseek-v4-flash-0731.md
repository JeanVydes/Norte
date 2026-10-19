# Token counting
A host budgets from the numbers the engine commits. This run reconciles the requests the engine made, the usage records it committed, and the provider's own payload — which the engine keeps verbatim — so a lost record or a dropped counter is visible.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request |
| system prefix | 8840 bytes, so the cache floor is cleared |
| user turns | 3 |
| verdict | one usage record per request |


## Reconciliation

|  |  |
| --- | --- |
| provider requests | 3 |
| usage records | 3 |
| complete records (input and output present) | 3 |
| one record per request | true |
| input tokens | 6265 |
| output tokens | 267 |
| reasoning tokens | 165 |
| cache read tokens | 3840 |
| cache write tokens | 0 |
| derived total tokens | 6532 |
| cost | 0.000435 |
| unpriced records | 0 |
| provider keys kept verbatim | completion_tokens_details, cost, cost_details, is_byok, prompt_tokens_details, total_tokens |


## Per record

| record | input | output | reasoning | cache read | cache write | derived total | cost | provider keys (verbatim) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | 2042 | 129 | 96 | 0 | 0 | 2171 | 0.000315 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |
| 2 | 2088 | 75 | 39 | 1792 | 0 | 2163 | 0.000075 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |
| 3 | 2135 | 63 | 30 | 2048 | 0 | 2198 | 0.000044 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |


## Reading

Every request left exactly one complete usage record, and the provider's keys are shown beside the normalized counters, so nothing it sent is hidden. The derived total is input plus output, which is what a budget should be built on.

