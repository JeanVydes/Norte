use std::fmt::{Debug, Formatter};

use serde::{Deserialize, Serialize};

use crate::domain::part::Part;
use crate::id::{ModelRef, ProviderId, WireFamily};

/// How widely a continuity token may be replayed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
#[non_exhaustive]
pub enum ContinuityScope {
    /// Bound to one exact provider/model pair.
    Exact(ModelRef),
    /// Bound to a model family within one provider.
    ModelFamily {
        provider: ProviderId,
        family: String,
    },
    /// Bound to one provider, across models.
    Provider(ProviderId),
    /// Replayable anywhere the payload is accepted.
    Any,
}

/// Opaque vendor state that must be sent back unchanged for the next request to
/// be valid.
///
/// Core never inspects the payload. Adapters build, merge and decide replay.
/// The payload is sensitive: debug output reports only its length, and events
/// never carry it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityToken {
    pub family: WireFamily,
    pub scope: ContinuityScope,
    pub schema_version: u16,
    #[serde(with = "crate::codec::bytes_base64")]
    payload: Vec<u8>,
}

impl ContinuityToken {
    #[must_use]
    pub fn new(
        family: WireFamily,
        scope: ContinuityScope,
        schema_version: u16,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            family,
            scope,
            schema_version,
            payload,
        }
    }

    /// Adapter-only access to the opaque payload.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.payload.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.payload.is_empty()
    }
}

impl Debug for ContinuityToken {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ContinuityToken")
            .field("family", &self.family)
            .field("scope", &self.scope)
            .field("schema_version", &self.schema_version)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

/// A streamed contribution to a continuity token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContinuitySemantics {
    Append,
    Replace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityFragment {
    pub family: WireFamily,
    pub scope: ContinuityScope,
    pub schema_version: u16,
    pub semantics: ContinuitySemantics,
    #[serde(with = "crate::codec::bytes_base64")]
    pub bytes: Vec<u8>,
}

impl ContinuityFragment {
    #[must_use]
    pub fn new(
        family: WireFamily,
        scope: ContinuityScope,
        schema_version: u16,
        semantics: ContinuitySemantics,
        bytes: Vec<u8>,
    ) -> Self {
        Self {
            family,
            scope,
            schema_version,
            semantics,
            bytes,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// What to do with one continuity token when replaying history into a target
/// model.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReplayDecision {
    Replay,
    Drop,
    /// Replace the part the token is attached to with a downgraded form.
    Downgrade(Box<Part>),
    /// Refuse to send a request known to be rejected.
    Block(String),
}

impl ReplayDecision {
    /// The default policy: exact model replays, anything else drops.
    ///
    /// Adapters override this with their own table where dropping would make
    /// the vendor reject the request.
    #[must_use]
    pub fn default_for(target: &ModelRef, token: &ContinuityToken) -> Self {
        match &token.scope {
            ContinuityScope::Exact(model) if model == target => Self::Replay,
            ContinuityScope::Any => Self::Replay,
            _ => Self::Drop,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn token(scope: ContinuityScope) -> ContinuityToken {
        ContinuityToken::new(
            WireFamily::new("test.wire").expect("family"),
            scope,
            1,
            b"opaque".to_vec(),
        )
    }

    #[test]
    fn debug_redacts_payload() {
        let token = token(ContinuityScope::Any);
        let rendered = format!("{token:?}");
        assert!(rendered.contains("payload_len"));
        assert!(!rendered.contains("opaque"));
    }

    #[test]
    fn exact_scope_replays_only_for_the_same_model() {
        let model = ModelRef::from_str("anthropic/claude-sonnet-4-5").expect("model");
        let other = ModelRef::from_str("anthropic/claude-opus-4-1").expect("model");
        let token = token(ContinuityScope::Exact(model.clone()));
        assert_eq!(
            ReplayDecision::default_for(&model, &token),
            ReplayDecision::Replay
        );
        assert_eq!(
            ReplayDecision::default_for(&other, &token),
            ReplayDecision::Drop
        );
    }
}
