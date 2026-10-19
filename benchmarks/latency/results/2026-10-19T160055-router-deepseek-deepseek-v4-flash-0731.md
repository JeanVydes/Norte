# Latency
One ask, measured twice: watching the stream (time to first token) and calling `finish()` without reading an event (turn time). `finish()` is not a second request — it drains the same stream — but a host that does not read events has no TTFT to report.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (the router decides) |
| rounds per mode | 3 |
| prompt | Write a four-line haiku about latency. |
| modes | streaming, buffered |


## Summary

| mode | rounds | ttft min | ttft p50 | total min | total p50 | total max | text deltas | reasoning | output tokens | cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| streaming | 3 | 2182.0 | 2507.1 | 12480.1 | 15266.9 | 19190.4 | 67 | 765 | 997 | 0.000255 |
| buffered | 3 | — | — | 11962.8 | 15328.7 | 17128.4 | 0 | 0 | 1536 | 0.000641 |

streaming turn time (ms)  `█▁▃`
buffered turn time (ms)  `█▅▁`

## Per round

| mode | round | ttft ms | total ms | text | reason | other | bytes | out tokens | tok/s | cost | note |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| streaming | 0 | 15019.5 | 19190.4 | 14 | 40 | 0 | 133 | 163 | 8.5 | 0.000035 |  |
| buffered | 0 | — | 17128.4 | 0 | 0 | 0 | 0 | 512 | 29.9 | 0.000271 |  |
| streaming | 1 | 2507.1 | 12480.1 | 22 | 339 | 0 | 101 | 383 | 30.7 | 0.000102 |  |
| buffered | 1 | — | 15328.7 | 0 | 0 | 0 | 0 | 512 | 33.4 | 0.000098 |  |
| streaming | 2 | 2182.0 | 15266.9 | 31 | 386 | 0 | 119 | 451 | 29.5 | 0.000118 |  |
| buffered | 2 | — | 11962.8 | 0 | 0 | 0 | 0 | 512 | 42.8 | 0.000272 |  |


## Reading

A turn here takes from 11962.8 to 19190.4. The streaming host sees its first token at `ttft`; the buffered host waits the whole turn before it can decide anything. A round with `note = no text delta arrived` spent the whole output budget before producing any answer text — latency the caller feels as nothing happening.

