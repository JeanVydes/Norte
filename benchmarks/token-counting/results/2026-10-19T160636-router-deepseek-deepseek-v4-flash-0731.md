# Token counting
A host budgets from the numbers the engine commits. This run reconciles the requests the engine made, the usage records it committed, and the provider's own payload — which the engine keeps verbatim — so a lost record or a dropped counter is visible.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (the router decides) |
| user turns | 4 |
| verdict | one usage record per request |


## Reconciliation

|  |  |
| --- | --- |
| provider requests | 4 |
| usage records | 4 |
| complete records (input and output present) | 4 |
| one record per request | true |
| input tokens | 670 |
| output tokens | 490 |
| reasoning tokens | 346 |
| cache read tokens | 0 |
| cache write tokens | 0 |
| derived total tokens | 1160 |
| cost | 0.000273 |
| unpriced records | 0 |
| provider keys kept verbatim | completion_tokens_details, cost, cost_details, is_byok, prompt_tokens_details, total_tokens |


## Per record

| record | input | output | reasoning | cache read | cache write | derived total | cost | provider keys (verbatim) |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 1 | 93 | 143 | 108 | 0 | 0 | 236 | 0.000077 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |
| 2 | 143 | 110 | 70 | 0 | 0 | 253 | 0.000061 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |
| 3 | 195 | 154 | 122 | 0 | 0 | 349 | 0.000085 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |
| 4 | 239 | 83 | 46 | 0 | 0 | 322 | 0.000049 | completion_tokens_details cost cost_details is_byok prompt_tokens_details total_tokens |


## Reading

Every request left exactly one complete usage record, and the provider's keys are shown beside the normalized counters, so nothing it sent is hidden. The derived total is input plus output, which is what a budget should be built on.

