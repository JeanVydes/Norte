//! Shared model parameter and wire quirk types.

mod generation_support;
/// Host-declared deviations from a wire protocol.
pub mod quirk;
mod supported_param;

pub use generation_support::{GenerationSupport, ParameterSupport};
pub use quirk::{Quirk, Quirks};
pub use supported_param::SupportedParam;
