# Fuzzing

A separate `cargo-fuzz` workspace for parser testing. It is excluded from
the main workspace (`exclude = ["fuzz"]` in the root `Cargo.toml`), so
`cargo test --workspace` and `cargo clippy --workspace` never touch it.

Run it with:

```sh
cd fuzz
cargo fuzz run <target>
```

Targets: `anthropic_messages`, `gemini_generate`, `openai_chat`,
`openai_responses`, `sse`.
