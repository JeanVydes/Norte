#![forbid(unsafe_code)]
#![doc = "Scripted models, fixture replay, request probes and conformance suites for `Norte`. Production code does not depend on this crate."]

/// Conformance suites that check a [`Model`](norte_core::Model) against its contract.
pub mod conformance;
/// Convenience builders for common messages.
pub mod fixtures;
/// Scripted models that replay a fixed script.
pub mod model;
/// A model that captures the requests it receives.
pub mod probe;

pub use conformance::model_conformance;
pub use fixtures::{model, system, user};
pub use model::{
    FailingModel, OpaqueContinuity, ScriptedModel, StrictContinuity, text_capabilities,
};
pub use norte_core::blob::InMemoryBlobStore;
pub use probe::ProbeModel;
