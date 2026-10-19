# Export/import fidelity
Every session is exported, repacked, imported into a fresh engine and re-exported; both engines then reconstruct the same next turn. Every seam below must be byte-for-byte identical, or the provider cache key moves and a cache miss is billed (FR-EXP-1, FR-EXP-2, FR-EXP-6, FR-RECON-12).


## Run

|  |  |
| --- | --- |
| mode | offline (deterministic, no provider) |
| wires | 4 |
| cases per wire | 200 |
| sessions total | 800 |
| generator seed | 1 |


## Per wire

| wire | cases | passed | failed | avg bytes | envelope | request | body | avg µs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| openai-responses | 200 | 200 | 0 | 815 | 200/200 | 200/200 | 200/200 | 1807 |
| openai-chat | 200 | 200 | 0 | 838 | 200/200 | 200/200 | 200/200 | 1512 |
| anthropic-messages | 200 | 200 | 0 | 839 | 200/200 | 200/200 | 200/200 | 1579 |
| gemini-generate | 200 | 200 | 0 | 812 | 200/200 | 200/200 | 200/200 | 1494 |

openai-responses  ████████████████████████████████  200
openai-chat  ████████████████████████████████  200
anthropic-messages  ████████████████████████████████  200
gemini-generate  ████████████████████████████████  200


## Failures

None. Every seam held on every session.


## Artifacts

Beside this report: a JSONL trail, one record per session with both `sha256` digests, and a CSV of the same table for a spreadsheet.

