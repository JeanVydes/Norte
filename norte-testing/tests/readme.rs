//! Compile-checks the wiring the README prints with the crates named one by one,
//! so the documentation cannot drift from the public API (NFR-2).

use std::sync::Arc;

use norte_core::{Capabilities, Message, Modality, ToolRegistry};
use norte_harness::InMemoryStore;
use norte_harness::{Harness, HarnessBuilder, InMemoryBlobStore};
use norte_openai::presets::openai;

fn readme_engine() -> Result<Harness, Box<dyn std::error::Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .build();
    let model = openai("gpt-5", "sk-test", capabilities)?;

    let engine = HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?;
    Ok(engine)
}

#[test]
fn the_readme_quickstart_wiring_compiles() {
    let _engine = readme_engine().expect("build engine");
    let _ = Message::user("Hello!");
}
