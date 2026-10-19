# Schema friction
When a tool call does not satisfy its schema, the engine commits the failure as a tool error and asks the model again. So a hard schema is paid for in requests. Each shape below is exercised with a trap ask, and the rejected calls are counted rather than assumed.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request; the elapsed column includes it |
| rounds per schema | 3 |
| shapes | 3 |


## Shapes

| schema | shape | trap | requests | calls | rejected | accepted | answered | called | input | output | cache read | cost | error |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| flat | one string field | none | 6 | 3 | 0 | 3 | 3/3 | 3/3 | 2628 | 332 | 1536 | 0.000277 |  |
| enum | one enum field (low, medium, high) | the ask insists on `urgent`, which is not in the enum | 4 | 1 | 1 | 0 | 3/3 | 1/3 | 1824 | 866 | 1024 | 0.000445 |  |
| nested | a nested guest object and a number, deny_unknown_fields | the ask insists on a `floor`, which the schema does not declare | 6 | 3 | 0 | 3 | 3/3 | 3/3 | 3270 | 1325 | 2048 | 0.000683 |  |

flat  ░░░░░░░░░░░░░░░░░░░░░░░░  0
enum  ████████████████████████  1
nested  ░░░░░░░░░░░░░░░░░░░░░░░░  0


## Per round

| schema | round | requests | calls | rejected | accepted | answered | input | output | cache read | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| flat | 0 | 2 | 1 | 0 | 1 | true | 876 | 94 | 512 | 0.000086 |
| flat | 1 | 2 | 1 | 0 | 1 | true | 876 | 133 | 512 | 0.000101 |
| flat | 2 | 2 | 1 | 0 | 1 | true | 876 | 105 | 512 | 0.000090 |
| enum | 0 | 1 | 0 | 0 | 0 | true | 435 | 209 | 256 | 0.000106 |
| enum | 1 | 2 | 1 | 1 | 0 | true | 954 | 407 | 512 | 0.000218 |
| enum | 2 | 1 | 0 | 0 | 0 | true | 435 | 250 | 256 | 0.000122 |
| nested | 0 | 2 | 1 | 0 | 1 | true | 1090 | 435 | 512 | 0.000247 |
| nested | 1 | 2 | 1 | 0 | 1 | true | 1090 | 544 | 768 | 0.000257 |
| nested | 2 | 2 | 1 | 0 | 1 | true | 1090 | 346 | 768 | 0.000180 |


## Reading

Across 16 requests the schema rejected 1 call(s). A rejected call is a whole extra request — the transcript, the schemas and the failed arguments again — so the count is a cost, not a footnote. The model called the tool in 7 of 9 rounds: a shape with zero rejected calls is a schema success only when that count is nonzero.

