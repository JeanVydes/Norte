# Contributing to Norte

Thank you for your interest in contributing! This repository is organized as a
Rust monorepo. Below you'll find how the workspace is structured and the preferred
workflow for contributing.

## Repository layout

- `norte/` — the facade: `norte-core` re-exported at the root, the `#[tool]`
  macro, and optional provider/helper modules. `norte/Cargo.toml` defines the
  features; `norte/src/lib.rs` defines the re-exports. Applications can depend on
  this crate alone. It provides no agent runtime.
- `norte-facade-smoke/` — a workspace package whose only dependency is `norte`.
  Its offline tests cover core types, macro metadata with implicit and explicit
  facade anchors, the default provider model and the opt-in `tools` feature,
  without starting a runtime or making network calls.
- `norte-core/` — the neutral domain model and adapter contracts, with no agent
  runtime or session store. `src/domain/` defines `Message`, `Part`, media and
  continuity types; `src/model.rs` defines `Model` and `ModelRequest`;
  `src/stream.rs` defines stream events and accumulation; `src/layer.rs` defines
  model-call layers. `src/tool/` owns tool declarations, the registry and
  execution; `src/blob.rs`, `src/media_adapter.rs` and `src/preprocess.rs` own
  the blob and media seams. `src/lib.rs` lists the public modules and re-exports.
  There are no `Session` or `Commit` domain types and no `norte-harness` crate.
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
- `examples/cookbook/` — recipe sources, not a current workspace member. Its
  manifest still references the retired harness; it is not a validated guide to
  the current API.
- `benchmarks/` — benchmark sources with their own manifests and documentation,
  not current workspace members.
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

- Core compiles with its full surface, and a wire crate compiles whole. The
  facade uses feature-gated re-exports to select optional crates; preserve the
  feature definitions in `norte/Cargo.toml`, including `google` as an alias for
  `gemini`, and `testing` as an opt-in separate from `full`. Wire crates also
  expose a `fuzz` feature for their fuzz targets.
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

- All tests in the current workspace:

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

For focused facade checks (no network or agent runtime):

```bash
cargo test --offline -p norte-facade-smoke
cargo test --offline -p norte
cargo test --offline -p norte --no-default-features
```

The smoke package deliberately declares no direct dependencies other than
`norte`; keep it that way so it catches facade paths that accidentally require
an application to import an implementation crate. It checks macro metadata, not
an async tool executor. Offline checks require dependencies to be cached locally.

Workspace commands do not validate the non-member cookbook or benchmark
manifests. Check those sources against the current contracts before running them;
benchmarks may contact paid models.

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
  model — the domain types, capability descriptors and ports in
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

The library crates are `norte`, `norte-core`, `norte-macros`, `norte-http`,
`norte-openai`, `norte-anthropic`, `norte-gemini`, `norte-tools`, `norte-media`,
`norte-mcp` and `norte-testing`. The facade's optional `testing` dependency pulls
`norte-testing` into the workspace through its path dependency. The
`norte-facade-smoke` workspace member is `publish = false`; cookbook and benchmark
sources are not current workspace members.

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
