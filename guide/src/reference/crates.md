# Crates

The workspace is one core, three wires, a handful of helpers, and — outside the
published set — an examples crate, a benchmark and a facade check.

| Crate | Depends on | What it owns |
| --- | --- | --- |
| `norte` | every crate below | The entrypoint. Re-exports `norte-core` at the root and every other crate as a module of its own name, each behind a feature, plus the merged `presets` and a `prelude`. It adds no behaviour of its own. |
| `norte-core` | nothing internal | The neutral domain model, the engine, the streaming pipeline, policies, layers, capabilities, and every contract (port). No vendor name, no model table, no external persistence. |
| `norte-http` | core | Shared HTTP transport, `VendorProfile`, auth schemes, secrets, quirks, SSE decoding, `HttpMedia`. |
| `norte-openai` | core, http | The `Responses` and `Chat Completions` wires, `OpenAiMedia`, presets. |
| `norte-anthropic` | core, http | The `Messages` wire, media, presets. |
| `norte-gemini` | core, http | `generateContent`, explicit cached content, media, presets. |
| `norte-tools` | core (http for media) | `read`, `write`, `edit`, `command` — the default host tools, and their policies' building blocks. |
| `norte-media` | core | Media preprocessing: `ImagePreprocessor`, `ImageOutput`. |
| `norte-mcp` | core | Model Context Protocol: client, server, stdio and channel transports, and remote tools as `AnyFunction`s. |
| `norte-testing` | core | Scripted model, request probe, recording observer, fixtures, conformance suites. |
| `norte-macros` | — | The procedural macros: `#[tool]`, `declare_function!`, `declare_tool!`. |

### The dependency rule

Dependencies point at core, and never sideways:

- `norte-core` pulls in no HTTP, database, vendor or concrete-tool code.
- No adapter depends on another adapter.

`norte` is the exception: it is a facade, it holds no logic, and it points at
everything with every edge optional but the core's.
`scripts/check-dependencies.sh` enforces the two rules above in CI, reading the
*normal* (not dev, not build) dependency tree; the facade is exempt.

## Not crates

| Path | What it is |
| --- | --- |
| `examples/cookbook` | Small, self-contained recipes: one file per topic, each wiring its own engine. Nothing is shared between them on purpose, so a recipe can be read and copied on its own. |
| `norte-facade-smoke` | A package whose only dependency is `norte`: every path and every macro in it starts at the facade. Its test mirrors the README quickstart. |
| `benchmarks/agent-cost` | What a coding agent costs on an existing crate, with one report per run under `results/`. |
| `docs/SRS.md` | The behavioural specification: the source of truth for what the engine promises, kept local and not committed. |
| `guide/` | This guide. |
| `fuzz/` | `cargo-fuzz` targets for the wire parsers. |

## Publishing

The eleven crates are published together, with `cargo publish --workspace`, which
goes in dependency order and leaves `norte` for last. They share the version in
`[workspace.package]`, and they are never published one by one from their
directory: a crate cannot point at a version the index does not have yet.
`examples/cookbook`, `benchmarks/agent-cost` and `norte-facade-smoke` are
`publish = false`. Every public item carries a doc comment — `cargo doc --workspace
--no-deps` with warnings denied is part of CI.
