//! Recipes for specific `Norte` tasks. Each one is a runnable example and
//! holds everything it needs, including its own model wiring:
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example approval
//! ```
//!
//! There is no shared code here on purpose: the only thing a recipe reuses is
//! the library. `tools` needs no key; the rest talk to a real model.
