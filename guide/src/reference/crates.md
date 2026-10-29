# Crates

The workspace is one core, one harness, three wires, a handful of helpers, and —
outside the published set — an examples crate, benchmarks and a facade check.

| Crate | Depends on | What it owns |
| --- | --- | --- |
| `norte` | every crate below | The entrypoint. Re-exports `norte-core` at the root (`norte::Harness` lives in `norte::harness`) and every other crate as a module of its own name, each behind a feature, plus the merged `presets` and a `prelude`. It adds no behaviour of its own. |
| `norte-core` | nothing internal | What a provider sees: `Message`, `Part`, `Role`, `Usage`, the `ModelRequest` and its `StreamEvent`s, and the contracts an adapter implements (`Model`, `BlobStore`, `MediaAdapter`, `Layer`, `Observer`, `ContinuityAdapter`). **It knows a list of messages; it does not know a graph.** |
| `norte-harness` | core | The runtime: `Harness` walks a session forward a turn at a time — the tool loop, the interceptors, the store it commits through, and the branch graph (`Session`, `Commit`, `Node`, `Conversation`) that only exists because something walks it. |
| `norte-http` | core | Shared HTTP transport, `VendorProfile`, auth schemes, secrets, quirks, SSE decoding, `HttpMedia`. |
| `norte-openai` | core, http | The `Responses` and `Chat Completions` wires, `OpenAiMedia`, presets. |
| `norte-anthropic` | core, http | The `Messages` wire, media, presets. |
| `norte-gemini` | core, http | `generateContent`, explicit cached content, media, presets. |
| `norte-tools` | core (http for media) | `read`, `write`, `edit`, `command` — the default host tools, and their policies' building blocks. |
| `norte-media` | core | Media preprocessing: `ImagePreprocessor`, `ImageOutput`. |
| `norte-mcp` | core | Model Context Protocol: client, server, stdio and channel transports, and remote tools as `AnyFunction`s. |
| `norte-testing` | core, harness (dev) | Scripted model, request probe, recording observer, fixtures, conformance suites — including `store_conformance`, which exercises the port the harness owns. |
| `norte-macros` | — | The procedural macros: `#[tool]`, `declare_function!`, `declare_tool!`. |

### The dependency rule

Dependencies point at core, and never sideways:

- `norte-core` pulls in no HTTP, database, vendor or concrete-tool code.
- No adapter depends on another adapter.
- No crate depends on `norte-harness` except `norte`, which exposes it, and
  `norte-testing`, whose library ships the conformance suite for the port the
  harness owns. The harness is a consumer of the core like any other; nothing in
  the core knows it exists, and a host is free to never use it.

`norte` is the exception: it is a facade, it holds no logic, and it points at
everything with every edge optional but the core's.
`scripts/check-dependencies.sh` enforces the three rules above in CI, reading the
*normal* (not dev, not build) dependency tree; the facade is exempt.

## Not crates

| Path | What it is |
| --- | --- |
| `examples/cookbook` | Small, self-contained recipes: nothing is shared between them on purpose, so a recipe can be read and copied on its own. The crate root holds the ones that use `norte-core` alone; `harness/` and `media/` hold the ones that need those crates. |
| `norte-facade-smoke` | A package whose only dependency is `norte`: every path and every macro in it starts at the facade. Its test mirrors the README quickstart. |
| `benchmarks/agent-cost` | What a coding agent costs on an existing crate, with one report per run under `results/`. |
| `guide/src/reference/requirements.md` | The requirements ledger: every id, the code that enforces it and the suite or CI job that checks it. |
| `guide/` | This guide. |
| `fuzz/` | `cargo-fuzz` targets for the wire parsers. |

## Publishing

The twelve crates are published together, with `cargo publish --workspace`, which
goes in dependency order and leaves `norte` for last. They share the version in
`[workspace.package]`, and they are never published one by one from their
directory: a crate cannot point at a version the index does not have yet.
`examples/cookbook`, `benchmarks/agent-cost` and `norte-facade-smoke` are
`publish = false`. Every public item carries a doc comment — `cargo doc --workspace
--no-deps` with warnings denied is part of CI.
