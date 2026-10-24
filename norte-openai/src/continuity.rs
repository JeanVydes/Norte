use std::collections::BTreeSet;

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

/// A call the model made but no result answers: dropping the token here would
/// leave the vendor with a tool call it considers open, and it would reject the
/// turn, so the adapter refuses it rather than sending a request known to be
/// rejected (FR-CONT-2, FR-CONT-4). A call that a result already answers is a
/// finished turn and must not block a replay.
const BLOCK_REASON: &str =
    "cannot replay vendor continuity across models while a tool result is pending";

fn mid_tool_loop(history: &[Message]) -> bool {
    let mut called = BTreeSet::new();
    let mut answered = BTreeSet::new();
    for message in history {
        for part in &message.parts {
            match part {
                Part::ToolCall(call) => {
                    called.insert(call.call_id);
                }
                Part::ToolResult(result) => {
                    answered.insert(result.call_id);
                }
                _ => {}
            }
        }
    }
    called.difference(&answered).next().is_some() || answered.difference(&called).next().is_some()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use norte_core::{CallId, Role, ToolCallPart, ToolResultPart};

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
    fn a_result_without_its_call_blocks_instead_of_dropping_tokens() {
        // The call the result answers is not in the history, so the vendor
        // would see a result it cannot place: refuse rather than drop.
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

    #[test]
    fn a_finished_tool_turn_drops_instead_of_blocking() {
        // A committed turn: the call and its result are both present, the model
        // answered, and the session was resumed. Nothing is owed, so a stale
        // token is dropped rather than blocking the whole resume.
        let call = CallId::new();
        let history = vec![
            Message::user("read a.txt"),
            Message::new(
                Role::Assistant,
                vec![Part::ToolCall(ToolCallPart {
                    call_id: call,
                    name: "read".to_owned(),
                    arguments: serde_json::json!({ "path": "a.txt" }),
                    continuity: None,
                })],
            ),
            Message::tool(vec![ToolResultPart::text(call, "contents")]),
            Message::assistant("done"),
        ];
        let decision = continuity().replay(
            &model_ref("openai/gpt-5-mini"),
            &history,
            &token("openai/gpt-5"),
        );
        assert_eq!(decision, ReplayDecision::Drop);
    }
}
