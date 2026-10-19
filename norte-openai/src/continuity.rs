use norte_core::{
    ContinuityAdapter, ContinuityFragment, ContinuityScope, ContinuitySemantics, ContinuityToken,
    Error, Message, ModelRef, Part, ReplayDecision, Result, WireFamily,
};
use serde_json::Value;

/// The token schema version this adapter writes and understands.
pub(crate) const SCHEMA_VERSION: u16 = 1;

/// Continuity interpreted by the `OpenAI` adapters.
///
/// The opaque payload is the `JSON` of the vendor state that must be resent
/// verbatim: a reasoning item with its `encrypted_content`, or a function-call
/// item's vendor id. Building and replaying are pure `JSON` operations, so the
/// bytes are stable across turns and never disturb the cached prefix.
#[derive(Clone, Debug)]
pub struct OpenAiContinuity {
    family: WireFamily,
    model: ModelRef,
}

impl OpenAiContinuity {
    /// An adapter for `family`, whose exact-scope tokens are bound to `model`.
    #[must_use]
    pub fn new(family: WireFamily, model: ModelRef) -> Self {
        Self { family, model }
    }

    /// The wire family whose state this adapter interprets.
    #[must_use]
    pub fn family(&self) -> &WireFamily {
        &self.family
    }

    /// Wraps a vendor `JSON` value as a `Replace` fragment bound to this model.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the value cannot be encoded.
    pub fn fragment(&self, wire: &Value) -> Result<ContinuityFragment> {
        let bytes = serde_json::to_vec(wire)?;
        Ok(ContinuityFragment::new(
            self.family.clone(),
            ContinuityScope::Exact(self.model.clone()),
            SCHEMA_VERSION,
            ContinuitySemantics::Replace,
            bytes,
        ))
    }

    /// Decodes a token back into the vendor `JSON` it was built from.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the payload is not valid `JSON`.
    pub fn decode(&self, token: &ContinuityToken) -> Result<Value> {
        serde_json::from_slice(token.payload()).map_err(Error::from)
    }
}

impl ContinuityAdapter for OpenAiContinuity {
    fn build(&self, wire: &Value) -> Result<ContinuityToken> {
        let payload = serde_json::to_vec(wire)?;
        Ok(ContinuityToken::new(
            self.family.clone(),
            ContinuityScope::Exact(self.model.clone()),
            SCHEMA_VERSION,
            payload,
        ))
    }

    fn merge(
        &self,
        base: Option<ContinuityToken>,
        fragment: ContinuityFragment,
    ) -> Result<ContinuityToken> {
        if fragment.family != self.family {
            return Err(Error::Codec(
                "openai continuity: fragment belongs to another wire family".to_owned(),
            ));
        }
        let payload = match fragment.semantics {
            ContinuitySemantics::Append => {
                let mut bytes = base
                    .map(|token| token.payload().to_vec())
                    .unwrap_or_default();
                bytes.extend_from_slice(&fragment.bytes);
                bytes
            }
            _ => fragment.bytes,
        };
        Ok(ContinuityToken::new(
            self.family.clone(),
            ContinuityScope::Exact(self.model.clone()),
            SCHEMA_VERSION,
            payload,
        ))
    }

    fn replay(
        &self,
        target: &ModelRef,
        history: &[Message],
        token: &ContinuityToken,
    ) -> ReplayDecision {
        if token.family != self.family {
            return ReplayDecision::Drop;
        }
        match ReplayDecision::default_for(target, token) {
            ReplayDecision::Replay => ReplayDecision::Replay,
            _ if mid_tool_loop(history) => ReplayDecision::Block(BLOCK_REASON.to_owned()),
            _ => ReplayDecision::Drop,
        }
    }
}

/// Dropping an encrypted reasoning item while a tool result is still owed makes
/// the vendor reject the turn, so the adapter refuses it rather than sending a
/// request known to be rejected (FR-CONT-2, FR-CONT-4).
const BLOCK_REASON: &str =
    "cannot replay vendor continuity across models while a tool result is pending";

fn mid_tool_loop(history: &[Message]) -> bool {
    history.iter().any(|message| {
        message
            .parts
            .iter()
            .any(|part| matches!(part, Part::ToolResult(_)))
    })
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use norte_core::{CallId, ToolResultPart};

    use super::*;

    fn model_ref(id: &str) -> ModelRef {
        ModelRef::from_str(id).expect("model")
    }

    fn continuity() -> OpenAiContinuity {
        OpenAiContinuity::new(
            WireFamily::new(crate::WIRE_RESPONSES).expect("family"),
            model_ref("openai/gpt-5"),
        )
    }

    fn token(model: &str) -> ContinuityToken {
        ContinuityToken::new(
            WireFamily::new(crate::WIRE_RESPONSES).expect("family"),
            ContinuityScope::Exact(model_ref(model)),
            1,
            b"opaque".to_vec(),
        )
    }

    #[test]
    fn an_exact_token_replays_for_its_own_model() {
        let decision = continuity().replay(&model_ref("openai/gpt-5"), &[], &token("openai/gpt-5"));
        assert_eq!(decision, ReplayDecision::Replay);
    }

    #[test]
    fn a_non_replayable_token_outside_a_tool_loop_is_dropped() {
        let decision = continuity().replay(
            &model_ref("openai/gpt-5-mini"),
            &[Message::user("hello")],
            &token("openai/gpt-5"),
        );
        assert_eq!(decision, ReplayDecision::Drop);
    }

    #[test]
    fn a_mid_tool_loop_switch_blocks_instead_of_dropping_tokens() {
        let history = vec![Message::tool(vec![ToolResultPart::text(
            CallId::new(),
            "ok",
        )])];
        let decision = continuity().replay(
            &model_ref("openai/gpt-5-mini"),
            &history,
            &token("openai/gpt-5"),
        );
        assert!(matches!(decision, ReplayDecision::Block(_)));
    }
}
