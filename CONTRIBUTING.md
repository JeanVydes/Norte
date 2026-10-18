# Contributing to RustyChain

Thank you for your interest in contributing! This repository is organized as a
Rust monorepo. Below you'll find how the workspace is structured and the preferred
workflow for contributing.

## Repository layout

- `rustychain-core/` — the neutral domain model, engine, streaming pipeline,
  policies, layers, capabilities and every contract (port). It carries
  abstractions only — no vendor names, model tables or external persistence —
  plus the in-memory `Store` and `BlobStore` implementations for running with
  nothing else.
- `rustychain-http/` — shared HTTP transport, `VendorProfile`, auth schemes and
  `HttpMedia`.
- `rustychain-openai/`, `rustychain-anthropic/`, `rustychain-gemini/` — the wire
  adapters and the native vendor presets.
- `rustychain-tools/` — the default host tool suite, including the `command`
  executor.
- `rustychain-media/` — media preprocessing helpers.
- `rustychain-mcp/` — Model Context Protocol integration.
- `rustychain-testing/` — the scripted model, fixture replayer, request probe
  and conformance suites.
- `rustychain-macros/` — the procedural macros used across the workspace.
- `docs/SRS.md` — the single source of truth for behaviour, kept local (it is
  deliberately not committed).
- top-level `Cargo.toml` — the workspace manifest.

## How to Contribute

1. **Fork the repository** and create your branch from `dev`.
2. **Make changes in the crate that owns the behaviour.** Core contracts live in
   `rustychain-core`; vendor translations live in the wire crates; host tools
   live in `rustychain-tools`.
3. **Write clear, well-documented code** and include tests. Keep changes scoped.
4. **Run the local checks** (see below).
5. **Open a pull request** with a clear description and reference related issues.
6. Be respectful and constructive in code reviews and discussions.

## Code style

- Format the whole workspace:

```bash
cargo fmt --all
```

- There are no cargo features; every crate builds with its full surface.
- Document public APIs with doc comments. Keep the public surface stable and
  documented.
- Clippy runs with `all` and `pedantic` denied (see `.github/workflows/rust.yml`).
  Do not add `#[allow(...)]` attributes; fix the lint.

## Local checks (pre-commit / pre-push)

These mirror CI so problems are caught before opening a PR.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo doc --workspace --no-deps
bash scripts/check-dependencies.sh
```

`scripts/check-dependencies.sh` enforces the dependency rule (NFR-ARCH-1): the
core depends on nothing internal, and wire crates never depend on each other.

## Building and running

```bash
# Build the whole workspace:
cargo build --workspace

# Build a single crate:
cargo build -p rustychain-core

# Run a single crate's tests:
cargo test -p rustychain-tools
```

## Running tests

- All tests, including the README wiring test and the adapter conformance suites:

```bash
cargo test --workspace
```

- Doctests:

```bash
cargo test --doc --workspace
```

Tests are hermetic: there are no live API keys and no external services. Provider
behaviour is pinned by recorded fixtures; live smoke tests are run separately on a
schedule (FR-TEST-2).

## Adding a new crate to the workspace

1. Add the crate folder at the repo root.
2. Add it to the top-level `Cargo.toml` under `[workspace]` → `members` and to
   `[workspace.dependencies]`.
3. Extend `scripts/check-dependencies.sh` if the crate introduces a new layer in
   the dependency rule.
4. Add conformance coverage if it implements an adapter or a port.

## API stability (NFR-2)

RustyChain follows [semantic versioning](https://semver.org/).

- **Patch** releases fix behaviour without changing the public API.
- **Minor** releases add public API in a backwards-compatible way.
- **Major** releases may change the public API. **Breaking changes to the neutral
  model — the domain types, capability descriptors, ports and engine surface in
  `rustychain-core` — require a major version.**

Practically:

- The neutral model is the stable surface. Internal modules may move in a minor
  release; re-exports from a crate root may not be removed in a minor release.
- Types marked `#[non_exhaustive]` may gain variants in a minor release; match
  them with a `_` arm.
- `capabilities_from_json` is a host-configuration bridge. Its schema tracks the
  neutral model and is versioned with it.
- Every public item carries a doc comment; run `cargo doc --workspace --no-deps`
  with warnings denied before publishing.

## Publishing crates

- Crates are published individually from their directory. Update the version in
  `[workspace.package]` (or per crate) and the changelog before publishing.
- The monorepo itself is not published as a single package.

## Reporting issues

- Use the issue tracker for bugs, feature requests, or questions.
- Provide as much detail as possible (logs, OS, steps to reproduce, crate name and
  version).

## Code of Conduct

- Be kind, inclusive, and respectful to others.

---

Happy coding!
