# long-context
A long session kept whole against the same session with its history compacted. The notes are host-authored and deterministic, so the two arms differ only in the context they carry: the full transcript against a host-written summary of it. Each arm asks the same question and the same follow-up.

Only what the provider reported is measured: input and output tokens, cache reads and writes, the bill and the wall-clock. Whether the compacted answer is as good is not a number this run produces, so both answers are quoted at the end for a reader to judge.


## Run

|  |  |
| --- | --- |
| route | deepseek/deepseek-v4-flash-0731 at https://openrouter.ai/api/v1 (baidu; pause 2000 ms) |
| pause | `BENCH_PAUSE_MS` before each request; the elapsed column includes it |
| exchanges in the notes | 32 |
| lines per exchange | 8 |
| system message | You are a concise assistant. Answer in one short sentence, in plain text, with no preamble. |
| cold question | In one sentence: what project do these notes describe, and what version number do they name? |
| warm follow-up | In one sentence: what single risk do the notes name? |
| context messages (whole) | 65 |
| context messages (compacted) | 2 |
| summary | Summary of the field notes: they describe the Norte engine, version 0.1.0, an append-only session engine with a commit log, a projection, a tool registry, a policy engine and a blob store. The notes record that each subsystem committed, replayed or sealed records for review, and that every observation was cross-checked against the rule set. The notes name one risk: a long prefix is re-sent on every request, so it is the context, not the answer, that grows. |

**Cold** means the first request of the arm, not a globally cold cache. The notes are deterministic, so an earlier run with the same prefix can leave a cache that this run's first request reads; the `cache read` column is the evidence.


## Summary

| arm | context messages | requests | input | cache read | cache write | output | cost | elapsed |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| whole | 65 | 2 | 30271 | 28672 | 0 | 418 | $0.000489 | 14.68 s |
| compacted | 2 | 2 | 480 | 0 | 0 | 148 | $0.000120 | 23.37 s |

whole input  ████████████████████████████  30271
compacted input  ░░░░░░░░░░░░░░░░░░░░░░░░░░░░  480


## Compaction

The cold request carried 15120 input tokens with the whole transcript and 226 with the summary — a reduction of 98.5%.

Across both requests the compacted arm cost $0.000120 against $0.000489 — a reduction of 75.5%.

A cache read is what a long, stable prefix can be worth: if the warm request of the whole arm reads a large share of its input from the provider's cache, the whole arm is cheaper than its token count suggests. Read the two rows above against each other — a policy that always compacts would throw that cache away.

A byte-identical prefix is necessary, not sufficient: the provider decides whether it has that prefix cached and which instance serves the request. A zero in `cache read` beside a byte-identical prefix means the provider did not serve one; it does not mean the prefix changed.


## Per request

| arm | step | # | messages | bytes | shared | byte-identical | shared digest | input | cache read | cache write | output | cost | elapsed |
| --- | --- | ---: | ---: | ---: | ---: | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| whole | cold | 1 | 66 | 72529 | — | — | — | 15120 | 14336 | 0 | 162 | $0.000224 | 5.11 s |
| whole | warm | 2 | 68 | 72738 | 66 | yes | 8c50fac838cf… | 15151 | 14336 | 0 | 256 | $0.000265 | 9.58 s |
| compacted | cold | 1 | 3 | 972 | — | — | — | 226 | 0 | 0 | 54 | $0.000050 | 14.07 s |
| compacted | warm | 2 | 5 | 1119 | 3 | yes | ef0892c79e61… | 254 | 0 | 0 | 94 | $0.000070 | 9.30 s |


## Prefix reuse

**whole**: the warm request carried 66 message(s) byte-identical to the cold one (shared digest `8c50fac838cf…`), which is the prefix a provider can serve from cache. Any cache read it reported is under that request's `cache read` column.

**compacted**: the warm request carried 3 message(s) byte-identical to the cold one (shared digest `ef0892c79e61…`), which is the prefix a provider can serve from cache. Any cache read it reported is under that request's `cache read` column.


## Answers

**whole**
1. The notes describe logging observations against the Norte engine with no version number named.

**compacted**
1. The Norte engine, version 0.1.0.
2. The risk is that a long prefix is re-sent on every request, so the context grows instead of the answer.


## Reading

The saving above is what compaction saves at request time. It is not the whole cost of a compaction policy: dropping history can lose what the question needs, and a summary written by a model is itself a request, paid for at the transcript's size. This run quotes both answers so those costs can be judged rather than assumed.

The summary is written by the host, not by the model, because Norte has no compaction policy — how history is dropped or summarized is a host decision (the engine's job is to commit what it is given, in order, and to re-send it unchanged). So this is the saving a host's policy could reach, measured against a transcript the host chose.

