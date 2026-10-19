# Latency
One ask, measured twice: watching the stream (time to first token) and calling `finish()` without reading an event (turn time). `finish()` is not a second request — it drains the same stream — but a host that does not read events has no TTFT to report.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request, outside the timer |
| rounds per mode | 5 |
| prompt | Write a four-line haiku about latency. |
| modes | streaming, buffered |


## Summary

| mode | rounds | ttft min | ttft p50 | total min | total p50 | total max | text deltas | reasoning | output tokens | cache read | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| streaming | 5 | 1182.8 | 1216.0 | 2580.5 | 3961.4 | 5253.9 | 28 | 617 | 1960 | 0 | 0.000823 |
| buffered | 5 | — | — | 2070.6 | 4431.9 | 5237.1 | 0 | 0 | 2059 | 0 | 0.000861 |

streaming turn time (ms)  `█▃▁▄▆`
buffered turn time (ms)  `█▆▅▁▆`

## Per round

| mode | round | ttft ms | total ms | text | reason | other | bytes | out tokens | cache read | tok/s | cost | note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| streaming | 0 | 2015.0 | 5253.9 | 0 | 177 | 0 | 0 | 512 | 0 | 97.5 | 0.000211 | no text delta arrived |
| buffered | 0 | — | 5237.1 | 0 | 0 | 0 | 0 | 512 | 0 | 97.8 | 0.000211 |  |
| streaming | 1 | 1182.8 | 3377.3 | 6 | 104 | 0 | 104 | 323 | 0 | 95.6 | 0.000138 |  |
| buffered | 1 | — | 4784.5 | 0 | 0 | 0 | 0 | 512 | 0 | 107.0 | 0.000211 |  |
| streaming | 2 | 1212.2 | 2580.5 | 17 | 56 | 0 | 120 | 211 | 0 | 81.8 | 0.000094 |  |
| buffered | 2 | — | 3916.5 | 0 | 0 | 0 | 0 | 410 | 0 | 104.7 | 0.000172 |  |
| streaming | 3 | 1664.6 | 3961.4 | 5 | 125 | 0 | 96 | 402 | 0 | 101.5 | 0.000168 |  |
| buffered | 3 | — | 2070.6 | 0 | 0 | 0 | 0 | 113 | 0 | 54.6 | 0.000056 |  |
| streaming | 4 | 1216.0 | 4861.0 | 0 | 155 | 0 | 0 | 512 | 0 | 105.3 | 0.000211 | no text delta arrived |
| buffered | 4 | — | 4431.9 | 0 | 0 | 0 | 0 | 512 | 0 | 115.5 | 0.000211 |  |


## Reading

A turn here takes from 2070.6 to 5253.9. The streaming host sees its first token at `ttft`; the buffered host waits the whole turn before it can decide anything. A round with `note = no text delta arrived` spent the whole output budget before producing any answer text — latency the caller feels as nothing happening.

