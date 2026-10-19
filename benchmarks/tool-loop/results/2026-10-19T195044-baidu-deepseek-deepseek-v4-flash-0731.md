# Tool loop
One tool ask, over a matrix of how many tools are declared and how long the transcript already is. A tool turn is not one request: the loop asks, runs, and asks again with the result, and every request carries the whole transcript plus every declared schema — including the tools never called.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request; the elapsed column includes it |
| rounds per configuration | 1 |
| ask | Count the words in this exact text: one two three four. You must call the word_count tool with that text. |


## Configuration

| config | schema bytes | first request bytes | requests | tool calls | tool errors | input tokens | output tokens | cache read | cost | median µs | error |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| tools=1 prior=0 | 312 | 695 | 2 | 1 | 0 | 891 | 94 | 512 | 0.000088 | 6783883 |  |
| tools=5 prior=0 | 2027 | 2410 | 2 | 1 | 0 | 1779 | 88 | 1024 | 0.000136 | 6584291 |  |
| tools=1 prior=3 | 312 | 1127 | 2 | 1 | 0 | 1059 | 57 | 512 | 0.000095 | 6718486 |  |
| tools=5 prior=3 | 2027 | 2842 | 2 | 1 | 0 | 1947 | 60 | 1536 | 0.000083 | 7448907 |  |

tools=1 prior=0  ███░░░░░░░░░░░░░  1
tools=5 prior=0  ████████████████  5
tools=1 prior=3  ███░░░░░░░░░░░░░  1
tools=5 prior=3  ████████████████  5


## Requests

Every provider request the loop made, with the usage the provider reported for it. The first carries the schemas; each later one carries the tool result and the whole prefix again.

| config | request | messages | bytes | digest | input | output | cache read | cost |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| tools=1 prior=0 | 1 | 1 | 695 | d6d47681281ecc78 | 414 | 79 | 256 | 0.000052 |
| tools=1 prior=0 | 2 | 3 | 973 | d3832cf4d36a357c | 477 | 15 | 256 | 0.000036 |
| tools=5 prior=0 | 1 | 1 | 2410 | 7c9992415e3b737e | 858 | 73 | 256 | 0.000108 |
| tools=5 prior=0 | 2 | 3 | 2688 | 572d2b02cd44617a | 921 | 15 | 768 | 0.000029 |
| tools=1 prior=3 | 1 | 7 | 1127 | 6f3bd33c777fff66 | 498 | 48 | 256 | 0.000051 |
| tools=1 prior=3 | 2 | 9 | 1405 | 3d74d7e8f8859b45 | 561 | 9 | 256 | 0.000044 |
| tools=5 prior=3 | 1 | 7 | 2842 | 891e9a210c9cf64c | 942 | 51 | 768 | 0.000046 |
| tools=5 prior=3 | 2 | 9 | 3120 | 3895068f3fdb5194 | 1005 | 9 | 768 | 0.000037 |


## Reading

The declared schemas alone are 2027 bytes at the widest and 312 at the narrowest, and those bytes are re-sent on every request of the loop — not once per turn. Read the `bytes` column per request beside `input tokens`: that is the schema and the transcript being paid for again each time the model is asked.

