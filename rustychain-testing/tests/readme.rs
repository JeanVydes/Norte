//! Compile-checks the wiring shown in `README.md`, so the documentation cannot
//! drift from the public API (NFR-2).

use std::sync::Arc;

use rustychain_core::{Capabilities, EngineBuilder, Message, Modality, ToolRegistry};
use rustychain_openai::presets::openai;
use rustychain_testing::{InMemoryBlobStore, InMemoryStore};

fn readme_engine() -> rustychain_core::Engine {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = openai("gpt-5", "sk-test", capabilities).expect("model");
    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()
        .expect("build engine")
}

#[test]
fn the_readme_quickstart_wiring_compiles() {
    let _engine = readme_engine();
    let _ = Message::user("Hello!");
}
