# Crates

Norte provides model and tool contracts, three provider wire adapters and shared
transport/media helpers. It does not include an agent runtime: the host owns its
turn loop, sessions, persistence, approvals and event consumers.

| Crate | Internal dependencies | Responsibility |
| --- | --- | --- |
| `norte` | core, macros; optional adapters/helpers | Facade: core types at the root, optional crates as modules, presets and a prelude. No execution logic. |
| `norte-core` | macros | Messages, parts, capabilities, model/tool ports, stream accumulation, continuity and model-call layers. |
| `norte-http` | core | HTTP deadlines and connection retries, bounded SSE decoding, vendor profiles, authentication and HTTP media. |
| `norte-openai` | core, http | Responses and Chat Completions wire adapters, Files API and native preset. |
| `norte-anthropic` | core, http | Messages wire adapter, Files API and native preset. |
| `norte-gemini` | core, http | generateContent wire adapter, explicit cached content, Files API and native preset. |
| `norte-tools` | core | Host tools such as read, write, edit and command. |
| `norte-media` | core | Media preprocessing. |
| `norte-mcp` | core | MCP client, server, transports and remote-tool integration. |
| `norte-testing` | core | Scripted and probe models, fixtures and model conformance checks. |
| `norte-macros` | none | Procedural macros for typed tool declarations. |

## Dependency rule

- Core does not depend on HTTP, databases, provider adapters or concrete tools.
- Concrete adapters do not depend on one another or on host runtime code.
- Test helpers are not normal dependencies of shipping adapters.
- The facade is the composition point; it may depend on all of these crates.

`scripts/check-dependencies.sh` checks normal dependencies, not dev/build edges.

## Validation and supporting directories

`norte-facade-smoke` is a non-published workspace member whose only direct
dependency is `norte`. It checks facade types, macros and selected features without
network access. It does not construct an agent runtime.

`fuzz/` contains a separate cargo-fuzz workspace for parser testing. `guide/`
contains this guide. `examples/cookbook/` and `benchmarks/` retain older host-runtime
examples and are not members of the current workspace; they are not validated by
`cargo test --workspace`.

## Publishing

Published crates share `[workspace.package].version`. The facade smoke package is
`publish = false`. Validate the actual workspace before release:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps
bash scripts/check-dependencies.sh
```
