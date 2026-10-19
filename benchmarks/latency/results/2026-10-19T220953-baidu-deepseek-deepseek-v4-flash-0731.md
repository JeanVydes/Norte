# Latency
One ask, measured twice: watching the stream (time to first token) and calling `finish()` without reading an event (turn time). `finish()` is not a second request — it drains the same stream — but a host that does not read events has no TTFT to report.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | 2000 ms before each request, outside the timer |
| rounds per mode | 10 |
| output budget | 2048 |
| prompt | Write a four-line haiku about latency. |
| modes | streaming, buffered |


## Summary

| mode | rounds | text rounds (stream) | first delta p50 | ttft text min | ttft text p50 | total min | total p50 | total max | text deltas | reasoning | output tokens | cache read | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| streaming | 10 | 6/10 | 1496.2 | 2478.3 | 7318.6 | 2562.7 | 7641.2 | 13758.6 | 44 | 4331 | 14533 | 0 | 0.005779 |
| buffered | 10 | — | — | — | — | 1748.5 | 3439.5 | 12195.6 | — | — | 9145 | 0 | 0.003680 |

streaming turn time (ms)  `█▅▇▃▆▄▁▃▃▇`
buffered turn time (ms)  `▁▅█▃▇▁▇▁▁▂`

## Per round

| mode | round | first delta ms | ttft text ms | total ms | text | reason | other | bytes | out tokens | cache read | tok/s | cost | note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| streaming | 0 | 1681.9 | — | 13758.6 | 0 | 692 | 0 | 0 | 2048 | 0 | 148.9 | 0.000809 | no text answer; the output budget may have gone to reasoning |
| buffered | 0 | — | — | 2842.8 | — | — | — | — | 227 | 0 | 79.9 | 0.000100 | deltas are not observed in buffered mode |
| streaming | 1 | 1557.7 | 9571.0 | 9679.8 | 7 | 457 | 0 | 114 | 1739 | 0 | 179.7 | 0.000689 |  |
| buffered | 1 | — | — | 9028.9 | — | — | — | — | 1473 | 0 | 163.1 | 0.000586 | deltas are not observed in buffered mode |
| streaming | 2 | 1508.9 | — | 12990.4 | 0 | 650 | 0 | 0 | 2048 | 0 | 157.7 | 0.000809 | no text answer; the output budget may have gone to reasoning |
| buffered | 2 | — | — | 12195.6 | — | — | — | — | 2034 | 0 | 166.8 | 0.000804 | deltas are not observed in buffered mode |
| streaming | 3 | 1248.9 | 6363.5 | 6546.9 | 7 | 287 | 0 | 107 | 1110 | 0 | 169.5 | 0.000444 |  |
| buffered | 3 | — | — | 5543.5 | — | — | — | — | 627 | 0 | 113.1 | 0.000256 | deltas are not observed in buffered mode |
| streaming | 4 | 1346.8 | — | 11641.3 | 0 | 584 | 0 | 0 | 2048 | 0 | 175.9 | 0.000809 | no text answer; the output budget may have gone to reasoning |
| buffered | 4 | — | — | 11257.7 | — | — | — | — | 2048 | 0 | 181.9 | 0.000809 | deltas are not observed in buffered mode |
| streaming | 5 | 1273.1 | 7548.3 | 7641.2 | 6 | 361 | 0 | 105 | 1173 | 0 | 153.5 | 0.000469 |  |
| buffered | 5 | — | — | 1748.5 | — | — | — | — | 73 | 0 | 41.8 | 0.000040 | deltas are not observed in buffered mode |
| streaming | 6 | 1410.5 | 2478.3 | 2562.7 | 10 | 61 | 0 | 94 | 189 | 0 | 73.7 | 0.000086 |  |
| buffered | 6 | — | — | 11528.0 | — | — | — | — | 2048 | 0 | 177.7 | 0.000809 | deltas are not observed in buffered mode |
| streaming | 7 | 1496.2 | 6485.8 | 6540.7 | 6 | 299 | 0 | 101 | 949 | 0 | 145.1 | 0.000381 |  |
| buffered | 7 | — | — | 2667.6 | — | — | — | — | 160 | 0 | 60.0 | 0.000074 | deltas are not observed in buffered mode |
| streaming | 8 | 1415.8 | 7318.6 | 7335.4 | 8 | 332 | 0 | 117 | 1181 | 0 | 161.0 | 0.000472 |  |
| buffered | 8 | — | — | 2172.1 | — | — | — | — | 110 | 0 | 50.6 | 0.000055 | deltas are not observed in buffered mode |
| streaming | 9 | 1634.2 | — | 12269.0 | 0 | 608 | 0 | 0 | 2048 | 0 | 166.9 | 0.000809 | no text answer; the output budget may have gone to reasoning |
| buffered | 9 | — | — | 3439.5 | — | — | — | — | 345 | 0 | 100.3 | 0.000146 | deltas are not observed in buffered mode |


## Reading

A turn here takes from 1748.5 to 13758.6. `first delta` is the first event of any kind, reasoning included; `ttft text` is the first answer token, which is the one a caller who waits for words feels. A round with `ttft text = —` spent the output budget before writing an answer; it is not a zero-latency answer. The buffered host has neither timestamp, nor a delta count, because it never reads an event; those cells are `—`. These are single-run samples on one route, not a significance test; compare runs that agree on model, provider, endpoint and commit.

