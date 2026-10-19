# Crates

The workspace is one core, three wires, a handful of helpers, and two things that
are neither — a cookbook and a set of benchmarks.

| Crate | Depends on | What it owns |
| --- | --- | --- |
| `rustychain-core` | nothing internal | The neutral domain model, the engine, the streaming pipeline, policies, layers, capabilities, and every contract (port). No vendor name, no model table, no external persistence. |
| `rustychain-http` | core | Shared HTTP transport, `VendorProfile`, auth schemes, secrets, quirks, SSE decoding, `HttpMedia`. |
| `rustychain-openai` | core, http | The `Responses` and `Chat Completions` wires, `OpenAiMedia`, presets. |
| `rustychain-anthropic` | core, http | The `Messages` wire, media, presets. |
| `rustychain-gemini` | core, http | `generateContent`, explicit cached content, media, presets. |
| `rustychain-tools` | core (http for media) | `read`, `write`, `edit`, `command` — the default host tools, and their policies' building blocks. |
| `rustychain-media` | core | Media preprocessing: `ImagePreprocessor`, `ImageOutput`. |
| `rustychain-mcp` | core | Model Context Protocol: client, server, stdio and channel transports, and remote tools as `AnyFunction`s. |
| `rustychain-testing` | core | Scripted model, request probe, recording observer, fixtures, conformance suites. |
| `rustychain-macros` | — | The procedural macros: `#[tool]`, `declare_function!`, `declare_tool!`. |

### The dependency rule

Dependencies point at core, and never sideways:

- `rustychain-core` pulls in no HTTP, database, vendor or concrete-tool code.
- No adapter depends on another adapter.

`scripts/check-dependencies.sh` enforces both in CI, reading the *normal* (not
dev, not build) dependency tree.

## Not crates

| Path | What it is |
| --- | --- |
| `examples/cookbook` | Small, self-contained recipes: one file per topic, each wiring its own engine. Nothing is shared between them on purpose, so a recipe can be read and copied on its own. |
| `benchmarks/agent-cost` | A measurement, not a recipe: what a coding agent costs, with a `results/` folder per benchmark. |
| `docs/SRS.md` | The behavioural specification, kept local and deliberately not committed. Source of truth for what the engine promises. |
| `guide/` | This guide. |
| `fuzz/` | `cargo-fuzz` targets for the wire parsers. |

## Publishing

Crates are published individually from their directory, with the version in
`[workspace.package]`. The monorepo is not published as one package, and every
public item carries a doc comment — `cargo doc --workspace --no-deps` with
warnings denied is part of CI.
