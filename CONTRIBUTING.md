# Contributing to RustyChain

Thank you for your interest in contributing! This repository is organized as a Rust monorepo. Below you'll find how the workspace is structured and the preferred workflow for contributing.

## Repository layout

- `rustychain-macros/` — a library crate containing procedural macros used by other crates in the workspace.
- `rustychain/` — the core library (crate) that provides the main functionality.
- `examples/` — a binary crate containing example programs and demos. Use this to run example apps.
- `rustychain-tools-suite/` — a library crate (and collection of tools) that builds on `rustychain`.
- top-level `Cargo.toml` — workspace manifest that includes the crates above as workspace members.

## How to Contribute

1. **Fork the repository** and create your branch from `main`.
2. **Make changes in the appropriate crate**: fix bugs or add features in `rustychain/` when they affect core behavior; add examples to `examples/`; add or improve tools in `rustychain-tools-suite/`.
3. **Write clear, well-documented code** and include tests when possible. Keep changes scoped to the crate that owns the functionality.
4. **Run formatting and linters** (see commands below) and include tests where applicable.
5. **Open a pull request** with a clear description of your changes and reference related issues.
6. Be respectful and constructive in code reviews and discussions.

## Code style
- Follow Rust best practices and formatting. Run `cargo fmt` across the workspace:

```bash
cargo fmt --all
```

- Document public APIs with doc comments. Keep public surface stable and documented.

## Local checks (pre-commit / pre-push)

Run the same checks CI runs to catch problems early. Use workspace flags for changes spanning crates, or `-p <name>` for a single crate.

- Clippy (fix mode for local auto-fixes; run per-package or workspace):

```bash
# Work on whole workspace (may be slower):
cargo clippy --workspace --all-targets --all-features

# Or for a single package (faster during development):
cargo clippy -p rustychain --all-targets --all-features
```

- Formatting check (CI uses `--check`):

```bash
cargo fmt --all -- --check
```

- Generate docs (for local verification):

```bash
cargo doc --workspace --no-deps --all-features
```

These commands mirror CI so running them locally helps catch issues before creating a PR.

## Building and running

- Build the entire workspace:

```bash
cargo build --workspace --all-features
```

- Build a specific crate (example: the core library):

```bash
cargo build -p rustychain --all-features
```

- Run the example crate (if it contains binaries):

```bash
# Run the default binary in `examples` crate
cargo run -p examples -- <args>

# Or run a specific binary by name (if defined in the crate)
cargo run -p examples --bin <binary-name> -- <args>
```

## Running tests

- Run all tests in the workspace:

```bash
cargo test --workspace --all-features
```

- Run tests for a single crate:

```bash
cargo test -p rustychain --all-features
```

- Run doctests for the workspace:

```bash
cargo test --doc --workspace --all-features
```

- Integration tests that need external services (e.g., Postgres with `pgvector`) may rely on Docker or `testcontainers`. If a test requires a local DB, export `DATABASE_URL` before running the tests:

```bash
export DATABASE_URL=postgresql://postgres:postgres@127.0.0.1:5432/test_db
cargo test -p rustychain --test <test_name> --all-features
```

## Adding a new crate to the workspace

1. Add the new crate folder at the repo root (e.g., `crates/my_crate` or top-level `my_crate/`).
2. Add the crate to the top-level `Cargo.toml` under the `[workspace]` -> `members` list.
3. Ensure CI and workspace scripts (if any) include the new crate where appropriate.

## Publishing crates

- If crates are intended to be published to crates.io, publish from the crate directory and follow semver. Update changelogs and the crate's `Cargo.toml` version before publishing.
- The monorepo itself is not published as a single package — individual crates are published separately.

## Reporting issues
- Use the issue tracker for bugs, feature requests, or questions.
- Provide as much detail as possible (logs, OS, steps to reproduce, crate name and version).

## Code of Conduct
- Be kind, inclusive, and respectful to others.

---

Happy coding!
