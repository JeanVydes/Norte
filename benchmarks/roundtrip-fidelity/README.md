# roundtrip-fidelity

Export/import fidelity, at scale: many generated sessions, four wires, and a
byte-for-byte comparison at every seam a host would cross.

The guarantee is the one that costs money when it breaks. A conversation is
exported, repacked, re-imported into a fresh engine, re-exported, and then both
engines reconstruct the same next turn. If any of those bytes differ, the
provider sees a prefix it never cached, and the cache — the whole point of an
append-only reconstruction — is gone.

Four seams are compared for every session, on four wires
(`openai-responses`, `openai-chat`, `anthropic-messages`, `gemini-generate`):

1. **Envelope** — `export -> to_json -> from_json -> to_json` is byte-stable.
2. **Re-export** — `export -> import -> export` is byte-identical to the first
   export. The session id travels, so the cache key does too.
3. **Reconstructed request** — the next turn's `ModelRequest`, after the import,
   serialises to the same bytes as the original's.
4. **Provider body** — the wire's own projection of that request is identical,
   byte for byte, and the exact `sha256` of both is recorded.

What is generated is deliberately varied and deliberately seeded: turns with
plain answers, turns with a tool call and its result, an ephemeral host message
that must reorder to the end, and random lengths — so the comparison is not one
happy path repeated. Same seed, same sessions, every machine.

## Run

```sh
cargo run -p norte-bench-roundtrip-fidelity -- --cases 200 --seed 1
```

Offline: no provider is called and no key is needed, so it runs in CI. Options:
`--cases <n>`, `--seed <n>`, `--report <p>`, `--jsonl <p>`, `--help`.

## The report

- **Run** — wires, cases per wire, sessions total, the seed.
- **Per wire** — cases, passed, failed, average bytes, and per-seam pass counts
  (envelope, request, body) with the average time.
- **Failures** — every seam that differed, with its digests, or none.
- **Artifacts** — where the JSONL and the CSV are.

Each run also writes `<stem>.jsonl`, `<stem>-cases.csv` and a manifest beside
the markdown.

## How to read it

- **Zero differences is the only passing number.** A single differing seam means
  a cache miss a host would not have seen coming.
- **The digests are the audit.** They are recorded per seam so a later run can be
  compared without trusting the summary.
