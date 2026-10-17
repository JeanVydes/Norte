#![forbid(unsafe_code)]
#![doc = "Reference stores and test doubles for `RustyChain`. Production code does not depend on this crate."]

pub mod blob;
pub mod conformance;
pub mod fixtures;
pub mod model;
pub mod observer;
pub mod probe;
pub mod store;

pub use blob::InMemoryBlobStore;
pub use conformance::{model_conformance, store_conformance};
pub use fixtures::{model, system, user};
pub use model::{FailingModel, OpaqueContinuity, ScriptedModel, StrictContinuity, text_capabilities};
pub use observer::RecordingObserver;
pub use probe::ProbeModel;
pub use store::InMemoryStore;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_reference_store_satisfies_the_contract() {
        store_conformance(&InMemoryStore::new()).await;
    }
}
