# Tool loop
One tool ask, over a matrix of how many tools are declared and how long the transcript already is. A tool turn is not one request: the loop asks, runs, and asks again with the result, and every request carries the whole transcript plus every declared schema — including the tools never called.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (the router decides) |
| rounds per configuration | 1 |
| ask | Count the words in this exact text: one two three four. You must call the word_count tool with that text. |


## Configuration

| config | schema bytes | first request bytes | requests | tool calls | tool errors | input tokens | output tokens | cache read | cost | median µs | error |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| tools=1 prior=0 | 312 | 640 | 2 | 1 | 0 | 891 | 86 | 0 | 0.000064 | 12639127 |  |
| tools=5 prior=0 | 2027 | 2355 | 2 | 1 | 0 | 1779 | 88 | 256 | 0.000080 | 11910623 |  |
| tools=1 prior=3 | 312 | 1072 | 3 | 2 | 0 | 1683 | 277 | 256 | 0.000157 | 18197108 |  |
| tools=5 prior=3 | 2027 | 2787 | 2 | 1 | 0 | 1869 | 88 | 0 | 0.000071 | 4397788 |  |

tools=1 prior=0  ███░░░░░░░░░░░░░  1
tools=5 prior=0  ████████████████  5
tools=1 prior=3  ███░░░░░░░░░░░░░  1
tools=5 prior=3  ████████████████  5


## Requests

Every provider request the loop made, with the usage the provider reported for it. The first carries the schemas; each later one carries the tool result and the whole prefix again.

| config | request | messages | bytes | digest | input | output | cache read | cost |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| tools=1 prior=0 | 1 | 1 | 640 | 77dee2f3bc29e052 | 414 | 71 | 0 | 0.000046 |
| tools=1 prior=0 | 2 | 3 | 918 | c404d6e78427f403 | 477 | 15 | 0 | 0.000018 |
| tools=5 prior=0 | 1 | 1 | 2355 | c7e1c78618c0c00c | 858 | 73 | 0 | 0.000057 |
| tools=5 prior=0 | 2 | 3 | 2633 | ddad3c9c497bf3eb | 921 | 15 | 256 | 0.000023 |
| tools=1 prior=3 | 1 | 7 | 1072 | a394100376efa249 | 498 | 48 | 0 | 0.000036 |
| tools=1 prior=3 | 2 | 9 | 1350 | 9200a512e972adf4 | 561 | 48 | 0 | 0.000028 |
| tools=1 prior=3 | 3 | 11 | 1628 | 4d159746af6d0604 | 624 | 181 | 256 | 0.000093 |
| tools=5 prior=3 | 1 | 7 | 2787 | 68c2079b21326afa | 942 | 73 | 0 | 0.000044 |
| tools=5 prior=3 | 2 | 9 | 3065 | c7fa7f683fb9d671 | 927 | 15 | 0 | 0.000027 |


## Reading

The declared schemas alone are 2027 bytes at the widest and 312 at the narrowest, and those bytes are re-sent on every request of the loop — not once per turn. Read the `bytes` column per request beside `input tokens`: that is the schema and the transcript being paid for again each time the model is asked.

