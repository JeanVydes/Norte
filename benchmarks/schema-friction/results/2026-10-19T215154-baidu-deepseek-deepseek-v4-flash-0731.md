# Schema friction
When a tool call does not satisfy its schema, the engine commits the failure as a tool error and asks the model again. So a hard schema is paid for in requests. Each shape below is exercised with a trap ask, and the rejected calls are counted rather than assumed.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request; the elapsed column includes it |
| rounds per schema | 5 |
| output budget | 1024 |
| shapes | 4 |


## Shapes

| schema | shape | trap | requests | calls | rejected | accepted | answered | called | input | output | cache read | cost | error |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| flat | one string field | none | 10 | 5 | 0 | 5 | 5/5 | 5/5 | 4389 | 447 | 2560 | 0.000422 |  |
| enum | one enum field (low, medium, high) | the ask insists on `urgent`, which is not in the enum | 7 | 2 | 1 | 1 | 5/5 | 2/5 | 3221 | 1520 | 1792 | 0.000785 |  |
| nested | a nested guest object and a number, deny_unknown_fields | the ask insists on a `floor`, which the schema does not declare | 10 | 5 | 0 | 5 | 5/5 | 5/5 | 5450 | 2679 | 3328 | 0.001332 |  |
| tagged | an internally tagged enum (`command: {kind: ...}`) | the ask insists the command is a plain string, which the tagged enum cannot accept | 16 | 11 | 6 | 5 | 5/5 | 5/5 | 11301 | 3295 | 8192 | 0.001720 |  |

flat  ░░░░░░░░░░░░░░░░░░░░░░░░  0
enum  ████░░░░░░░░░░░░░░░░░░░░  1
nested  ░░░░░░░░░░░░░░░░░░░░░░░░  0
tagged  ████████████████████████  6


## Rejected calls

**enum**
- ×1: invalid arguments for tool set_priority: unknown variant `urgent`, expected one of `low`, `medium`, `high`

**tagged**
- ×5: invalid arguments for tool run_command: invalid type: string "echo hi", expected internally tagged enum CommandLine
- ×1: invalid arguments for tool run_command: invalid type: string "{\"kind\": \"program\", \"program\": \"echo\", \"args\": [\"hi\"]}", expected internally tagged enum CommandLine


## Per round

| schema | round | requests | calls | rejected | accepted | answered | input | output | cache read | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| flat | 0 | 2 | 1 | 0 | 1 | true | 876 | 78 | 512 | 0.000080 |
| flat | 1 | 2 | 1 | 0 | 1 | true | 876 | 73 | 512 | 0.000078 |
| flat | 2 | 2 | 1 | 0 | 1 | true | 885 | 108 | 512 | 0.000093 |
| flat | 3 | 2 | 1 | 0 | 1 | true | 876 | 94 | 512 | 0.000086 |
| flat | 4 | 2 | 1 | 0 | 1 | true | 876 | 94 | 512 | 0.000086 |
| enum | 0 | 1 | 0 | 0 | 0 | true | 435 | 353 | 256 | 0.000162 |
| enum | 1 | 1 | 0 | 0 | 0 | true | 435 | 208 | 256 | 0.000105 |
| enum | 2 | 2 | 1 | 1 | 0 | true | 954 | 271 | 512 | 0.000165 |
| enum | 3 | 1 | 0 | 0 | 0 | true | 435 | 303 | 256 | 0.000142 |
| enum | 4 | 2 | 1 | 0 | 1 | true | 962 | 385 | 512 | 0.000210 |
| nested | 0 | 2 | 1 | 0 | 1 | true | 1090 | 492 | 256 | 0.000301 |
| nested | 1 | 2 | 1 | 0 | 1 | true | 1090 | 441 | 768 | 0.000217 |
| nested | 2 | 2 | 1 | 0 | 1 | true | 1090 | 506 | 768 | 0.000242 |
| nested | 3 | 2 | 1 | 0 | 1 | true | 1090 | 468 | 768 | 0.000227 |
| nested | 4 | 2 | 1 | 0 | 1 | true | 1090 | 772 | 768 | 0.000346 |
| tagged | 0 | 3 | 2 | 1 | 1 | true | 2064 | 427 | 1280 | 0.000273 |
| tagged | 1 | 3 | 2 | 1 | 1 | true | 2100 | 1162 | 1536 | 0.000532 |
| tagged | 2 | 3 | 2 | 1 | 1 | true | 2064 | 446 | 1536 | 0.000249 |
| tagged | 3 | 3 | 2 | 1 | 1 | true | 2076 | 473 | 1536 | 0.000261 |
| tagged | 4 | 4 | 3 | 2 | 1 | true | 2997 | 787 | 2304 | 0.000406 |


## Reading

Across 43 requests the schema rejected 7 call(s). A rejected call is a whole extra request — the transcript, the schemas and the failed arguments again — so the count is a cost, not a footnote. The model called the tool in 17 of 20 rounds: a shape with zero rejected calls is a schema success only when that count is nonzero. The `tagged` row is the shape the `command` tool uses; the other enum row is a scalar enum and does not exercise a discriminator.

