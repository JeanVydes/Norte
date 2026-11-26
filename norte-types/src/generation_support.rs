use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::SupportedParam;

/// Known support for generation parameters. Omitted entries in a partial list
/// have unknown support; omitted entries in a complete list are unsupported.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenerationSupport {
    /// Parameters confirmed to be supported.
    pub supported: BTreeSet<SupportedParam>,
    /// Whether the list describes all parameters accepted by the model.
    pub complete: bool,
}

/// The information a host has about one generation parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParameterSupport {
    /// The host confirms that the model accepts this parameter.
    Supported,
    /// The host confirms that the model rejects this parameter.
    Unsupported,
    /// The host has no declaration for this parameter.
    Unknown,
}

impl GenerationSupport {
    /// Looks up support without treating missing data as a prohibition.
    #[must_use]
    pub fn support(&self, param: SupportedParam) -> ParameterSupport {
        if self.supported.contains(&param) {
            ParameterSupport::Supported
        } else if self.complete {
            ParameterSupport::Unsupported
        } else {
            ParameterSupport::Unknown
        }
    }
}
