# Contributing to Norte

Thank you for your interest in contributing! This repository is organized as a
Rust monorepo. Below you'll find how the workspace is structured and the preferred
workflow for contributing.

## Repository layout

- `norte/` — the facade: `norte-core` re-exported at the root and every other
  crate as a module of its own name, behind features. It adds no behaviour, and it
  is the crate an application should depend on.
- `norte-facade-smoke/` — a package whose only dependency is `norte`, which uses
  the facade the way an application would.
- `norte-core/` — the neutral domain model, streaming pipeline, layers,
  capabilities and every contract (port). It carries abstractions only — no
  vendor names, model tables or external persistence — and **no agent
  runtime**: nothing in it knows there is a turn loop. `Message`, `Part`,
  `Session` and `Commit` are here because a provider serialises them; the
  `BlobStore` port is here because `MediaAdapter` names it.
- `norte-harness/` — one reference agent loop over those contracts: the `Harness`
  walks a session forward a turn at a time, with the tool loop, approvals and
  the session store. It is deliberately *a* harness and not *the* harness; a
  host that wants a different turn policy depends on `norte-core` alone.
- `norte-http/` — shared HTTP transport, `VendorProfile`, auth schemes and
  `HttpMedia`.
- `norte-openai/`, `norte-anthropic/`, `norte-gemini/` — the wire
  adapters and the native vendor presets.
- `norte-tools/` — the default host tool suite, including the `command`
  executor.
- `norte-media/` — media preprocessing helpers.
- `norte-mcp/` — Model Context Protocol integration.
- `norte-testing/` — the scripted model, fixture replayer, request probe
  and conformance suites.
- `norte-macros/` — the procedural macros used across the workspace.
- `examples/cookbook/` — small, self-contained recipes, nothing shared between
  them on purpose. The crate root holds the ones that use `norte-core` alone;
  `harness/` and `media/` hold the ones that need those crates.
- `benchmarks/` — measurements against a real model, one folder per benchmark,
  each with its own README and a `results/` folder the run writes into.
- `guide/` — the multi-page user guide, written in Markdown and built with
  [mdBook](https://rust-lang.github.io/mdBook/) (`mdbook serve guide`). CI
  publishes it to GitHub Pages from `dev`; a pull request only builds it.
- `guide/src/reference/requirements.md` — the requirements ledger: every id, the
  code that enforces it and the suite or CI job that checks it. The code is the
  source of truth; the ledger records the promise it implements.
- top-level `Cargo.toml` — the workspace manifest.

## How to Contribute

1. **Fork the repository** and create your branch from `dev`.
2. **Make changes in the crate that owns the behaviour.** Core contracts live in
   `norte-core`; vendor translations live in the wire crates; host tools
   live in `norte-tools`.
3. **Write clear, well-documented code** and include tests. Keep changes scoped.
4. **Run the local checks** (see below).
5. **Open a pull request** with a clear description and reference related issues.
6. Be respectful and constructive in code reviews and discussions.

## Code style

- Format the whole workspace:

```bash
cargo fmt --all
```

- There is no conditional compilation inside a crate: core compiles with its full
  surface, and a wire crate compiles whole. The only features in the workspace
  select *which crates* are pulled in (`norte`), plus the `fuzz` feature the wire
  crates expose to their fuzz targets.
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

The guide is not part of the Rust gate; build it with
`mdbook build guide` (or read it live with `mdbook serve guide`). CI builds it in
`.github/workflows/pages.yml`.

## Building and running

```bash
# Build the whole workspace:
cargo build --workspace

# Build a single crate:
cargo build -p norte-core

# Run a single crate's tests:
cargo test -p norte-tools
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

A benchmark talks to a paid model, so `cargo test --workspace` type-checks and
builds it but never runs it. Its results are written by the run, under its own
`results/`.

## Adding a new crate to the workspace

1. Add the crate folder at the repo root — or under `examples/` for something
   runnable and under `benchmarks/` for something measured.
2. Add it to the top-level `Cargo.toml` under `[workspace]` → `members` and to
   `[workspace.dependencies]`.
3. Extend `scripts/check-dependencies.sh` if the crate introduces a new layer in
   the dependency rule.
4. Add conformance coverage if it implements an adapter or a port.

## API stability (NFR-2)

Norte follows [semantic versioning](https://semver.org/).

- **Patch** releases fix behaviour without changing the public API.
- **Minor** releases add public API in a backwards-compatible way.
- **Major** releases may change the public API. **Breaking changes to the neutral
  model — the domain types, capability descriptors, ports and engine surface in
  `norte-core` — require a major version.**

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

The workspace publishes eleven crates; `examples/cookbook`, `benchmarks/agent-cost`
and `norte-facade-smoke` are `publish = false` and never leave the repository.

One command does the whole set, in dependency order, and it works before anything
is on the registry: `cargo publish --workspace` stands up a temporary local
registry so each crate is verified against the ones published a moment earlier in
the same run.

```bash
cargo publish --workspace --dry-run   # the whole set: packaged, verified, not uploaded
cargo publish --workspace             # for real
```

The release checklist:

1. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
   `cargo test --workspace`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`.
2. `cargo publish --workspace --dry-run` clean, with eleven `Uploading` lines and
   no crate skipped.
3. The version bumped in `[workspace.package]` and in every `norte-*`
   `version = "…"` entry of the workspace manifest, plus a `CHANGELOG.md` entry.
4. `cargo login` once with a crates.io API token for the publishing account;
   `cargo publish` asks for the one-time code if 2FA is enabled.
5. After the first release, point the README's install block at the registry
   (`norte = { version = "0.1" }`) instead of at the git branch.

Every published crate carries `license`, `description`, `homepage`, `repository`,
`readme`, `keywords` and `categories`; `cargo publish` warns when one is missing.
A copy of the workspace `LICENSE` sits next to each published crate's
`Cargo.toml`, so the license text travels inside the published tarball.

## Reporting issues

- Use the issue tracker for bugs, feature requests, or questions.
- Provide as much detail as possible (logs, OS, steps to reproduce, crate name and
  version).

## Code of Conduct

- Be kind, inclusive, and respectful to others.

---

Happy coding!
