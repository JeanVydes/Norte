# Tool loop
One tool ask, over a matrix of how many tools are declared and how long the transcript already is. A tool turn is not one request: the loop asks, runs, and asks again with the result, and every request carries the whole transcript plus every declared schema — including the tools never called.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request; the elapsed column includes it |
| rounds per configuration | 3 |
| ask | Count the words in this exact text: one two three four. You must call the word_count tool with that text. |


## Configuration

| config | schema bytes | first request bytes | requests | tool calls | tool errors | input tokens | output tokens | cache read | cost | cost/round | median µs | error |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| tools=1 prior=0 | 312 | 694 | 6 | 3 | 0 | 2673 | 274 | 1280 | 0.000293 | 0.000098 | 7060575 |  |
| tools=5 prior=0 | 2027 | 2409 | 6 | 3 | 0 | 5337 | 260 | 4096 | 0.000279 | 0.000093 | 7140859 |  |
| tools=1 prior=3 | 312 | 1126 | 6 | 3 | 0 | 3177 | 259 | 2304 | 0.000224 | 0.000075 | 6983396 |  |
| tools=5 prior=3 | 2027 | 2841 | 6 | 3 | 0 | 5841 | 259 | 4608 | 0.000280 | 0.000093 | 6885010 |  |

tools=1 prior=0  ███░░░░░░░░░░░░░  1
tools=5 prior=0  ████████████████  5
tools=1 prior=3  ███░░░░░░░░░░░░░  1
tools=5 prior=3  ████████████████  5


## Requests

Every provider request the loop made, with the usage the provider reported for it. The first carries the schemas; each later one carries the tool result and the whole prefix again.

| config | request | messages | bytes | digest | input | output | cache read | cost |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| tools=1 prior=0 | 1 | 1 | 694 | 802a4760a1cb3665 | 414 | 77 | 256 | 0.000052 |
| tools=1 prior=0 | 2 | 3 | 972 | b1098fd3ba863c5a | 477 | 15 | 256 | 0.000036 |
| tools=1 prior=0 | 1 | 1 | 694 | 3a3aff5bf9e5567a | 414 | 76 | 256 | 0.000051 |
| tools=1 prior=0 | 2 | 3 | 972 | b671a1239cc4b97a | 477 | 15 | 256 | 0.000036 |
| tools=1 prior=0 | 1 | 1 | 694 | 2e91da48edeecbc2 | 414 | 76 | 256 | 0.000051 |
| tools=1 prior=0 | 2 | 3 | 972 | f866876a2948780a | 477 | 15 | 0 | 0.000068 |
| tools=5 prior=0 | 1 | 1 | 2409 | 404a4f97acb82a9a | 858 | 69 | 768 | 0.000042 |
| tools=5 prior=0 | 2 | 3 | 2687 | 39a8da4811beedd9 | 921 | 15 | 256 | 0.000093 |
| tools=5 prior=0 | 1 | 1 | 2409 | 27c75b6cb3f25505 | 858 | 74 | 768 | 0.000044 |
| tools=5 prior=0 | 2 | 3 | 2687 | 497b92479436ec0c | 921 | 15 | 768 | 0.000029 |
| tools=5 prior=0 | 1 | 1 | 2409 | cbadc1efdc7f7c6d | 858 | 72 | 768 | 0.000043 |
| tools=5 prior=0 | 2 | 3 | 2687 | eef7946995610568 | 921 | 15 | 768 | 0.000029 |
| tools=1 prior=3 | 1 | 7 | 1126 | 142f7589e612b197 | 498 | 66 | 256 | 0.000058 |
| tools=1 prior=3 | 2 | 9 | 1404 | e274e61487be0058 | 561 | 18 | 512 | 0.000015 |
| tools=1 prior=3 | 1 | 7 | 1126 | 1fc62c8ba9ca4741 | 498 | 73 | 256 | 0.000061 |
| tools=1 prior=3 | 2 | 9 | 1404 | 48246b60146b0e50 | 561 | 15 | 512 | 0.000014 |
| tools=1 prior=3 | 1 | 7 | 1126 | ceb3683aa40751c1 | 498 | 71 | 256 | 0.000060 |
| tools=1 prior=3 | 2 | 9 | 1404 | e18d231777c03379 | 561 | 16 | 512 | 0.000015 |
| tools=5 prior=3 | 1 | 7 | 2841 | 33cb98c9ad56532e | 942 | 70 | 768 | 0.000053 |
| tools=5 prior=3 | 2 | 9 | 3119 | 7c65cb212b955d4b | 1005 | 15 | 768 | 0.000040 |
| tools=5 prior=3 | 1 | 7 | 2841 | 91a329d1c87b0e05 | 942 | 72 | 768 | 0.000054 |
| tools=5 prior=3 | 2 | 9 | 3119 | fd2c66368a5506c2 | 1005 | 15 | 768 | 0.000040 |
| tools=5 prior=3 | 1 | 7 | 2841 | 6973b39e42c61fb4 | 942 | 72 | 768 | 0.000054 |
| tools=5 prior=3 | 2 | 9 | 3119 | 5f6f3c6529e9e2bc | 1005 | 15 | 768 | 0.000040 |


## Reading

The declared schemas alone are 2027 bytes at the widest and 312 at the narrowest, and those bytes are re-sent on every request of the loop — not once per turn. Read the `bytes` column per request beside `input tokens`: that is the schema and the transcript being paid for again each time the model is asked.

The `cost` column is not monotone across configurations: the provider's cache read differs by prefix and by round, so a wider configuration can be billed less than a narrower one. `cost/round` normalizes the number of rounds, but not the cache. The monotone evidence for the cost of widening the surface is `schema bytes` and `input tokens`; this run is a small sample, not a price model.

