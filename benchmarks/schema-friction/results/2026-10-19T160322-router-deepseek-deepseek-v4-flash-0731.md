# Schema friction
When a tool call does not satisfy its schema, the engine commits the failure as a tool error and asks the model again. So a hard schema is paid for in requests. Each shape below is exercised with a trap ask, and the rejected calls are counted rather than assumed.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (the router decides) |
| rounds per schema | 3 |
| shapes | 3 |


## Shapes

| schema | shape | trap | requests | calls | rejected | accepted | answered | input | output | cost | error |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| flat | one string field | none | 6 | 3 | 0 | 3 | 3/3 | 2628 | 443 | 0.000250 |  |
| enum | one enum field (low, medium, high) | the ask insists on `urgent`, which is not in the enum | 3 | 0 | 0 | 0 | 3/3 | 1305 | 1161 | 0.000413 |  |
| nested | a nested guest object and a number, deny_unknown_fields | the ask insists on a `floor`, which the schema does not declare | 9 | 6 | 5 | 1 | 1/3 | 5918 | 3007 | 0.001152 |  |

flat  ░░░░░░░░░░░░░░░░░░░░░░░░  0
enum  ░░░░░░░░░░░░░░░░░░░░░░░░  0
nested  ████████████████████████  5


## Per round

| schema | round | requests | calls | rejected | accepted | answered | input | output | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| flat | 0 | 2 | 1 | 0 | 1 | true | 876 | 231 | 0.000110 |
| flat | 1 | 2 | 1 | 0 | 1 | true | 876 | 79 | 0.000052 |
| flat | 2 | 2 | 1 | 0 | 1 | true | 876 | 133 | 0.000087 |
| enum | 0 | 1 | 0 | 0 | 0 | true | 435 | 464 | 0.000138 |
| enum | 1 | 1 | 0 | 0 | 0 | true | 435 | 431 | 0.000130 |
| enum | 2 | 1 | 0 | 0 | 0 | true | 435 | 266 | 0.000146 |
| nested | 0 | 2 | 1 | 1 | 0 | false | 1164 | 1024 | 0.000333 |
| nested | 1 | 5 | 4 | 3 | 1 | true | 3612 | 1044 | 0.000508 |
| nested | 2 | 2 | 1 | 1 | 0 | false | 1142 | 939 | 0.000311 |


## Reading

Across 18 requests the schema rejected 5 call(s). A rejected call is a whole extra request — the transcript, the schemas and the failed arguments again — so the count is a cost, not a footnote. A shape with zero rejected calls is a schema the model got right the first time.

