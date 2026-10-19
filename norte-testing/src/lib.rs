#![forbid(unsafe_code)]
#![doc = "Scripted models, fixture replay, request probes and conformance suites for `Norte`. Production code does not depend on this crate."]

pub mod conformance;
pub mod fixtures;
pub mod model;
pub mod observer;
pub mod probe;

pub use conformance::{model_conformance, store_conformance};
pub use fixtures::{model, system, user};
pub use model::{
    FailingModel, OpaqueContinuity, ScriptedModel, StrictContinuity, text_capabilities,
};
pub use norte_core::memory::{InMemoryBlobStore, InMemoryStore};
pub use observer::RecordingObserver;
pub use probe::ProbeModel;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_reference_store_satisfies_the_contract() {
        store_conformance(&InMemoryStore::new()).await;
    }
}
