use std::collections::BTreeMap;
use std::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::{Capabilities, ReasoningRequest};
use crate::domain::continuity::{ContinuityFragment, ContinuityToken, ReplayDecision};
use crate::domain::message::Message;
use crate::error::{Error, Result};
use crate::id::{ModelRef, ToolName};
use crate::stream::StreamEvent;
use crate::tool::ToolSpec;

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send + 'static>>;

/// Request extension key carrying the originating session id, so adapters can
/// derive a stable provider cache key without inventing their own identity.
pub const SESSION_EXTENSION: &str = "session";

/// Session-metadata key carrying the effective request options of the last turn
/// (FR-EXP-7). It is engine-managed; hosts must not write it.
pub const TURN_OPTIONS_KEY: &str = "rustychain.turn_options";

/// Session-metadata key carrying the tool results already produced for an
/// assistant message whose remaining calls parked for approval, so resuming
/// does not run those calls a second time (FR-APPR-8). It is engine-managed;
/// hosts must not write it, and it is removed once the tool message commits.
pub const PARTIAL_TOOLS_KEY: &str = "rustychain.partial_tools";

/// Sampling and output controls shared by every adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// JSON Schema the output must satisfy, when the model supports structured
    /// output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl GenerationParams {
    /// True when every field is unset, i.e. the parameters change nothing.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Overlays `self` on `base`: every set field wins, every unset field
    /// inherits from `base` (FR-ENG-7).
    ///
    /// An empty `stop_sequences` means "unset" and therefore inherits; a
    /// non-empty list replaces `base` entirely.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        Self {
            temperature: self.temperature.or(base.temperature),
            top_p: self.top_p.or(base.top_p),
            top_k: self.top_k.or(base.top_k),
            max_output_tokens: self.max_output_tokens.or(base.max_output_tokens),
            stop_sequences: if self.stop_sequences.is_empty() {
                base.stop_sequences.clone()
            } else {
                self.stop_sequences.clone()
            },
            seed: self.seed.or(base.seed),
            output_schema: self
                .output_schema
                .clone()
                .or_else(|| base.output_schema.clone()),
        }
    }
}

/// Per-turn request overrides (FR-ENG-7).
///
/// Layered least-to-most specific — engine defaults ← session defaults ← turn
/// overrides — and merged per field: a field set here replaces that field only;
/// an unset field inherits from the layer below. The effective options are
/// exactly what the adapter receives (FR-ENG-9).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnOptions {
    /// Generation-parameter overrides; unset fields inherit.
    #[serde(default, skip_serializing_if = "GenerationParams::is_default")]
    pub params: GenerationParams,
    /// Reasoning override; `None` inherits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningRequest>,
    /// Exposed tool subset by name; `None` exposes every registered tool
    /// (FR-ENG-8). Order is canonicalized at request time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolName>>,
    /// Vendor request extras, keyed `<wire-family>:<field>` (for example
    /// `google.gemini:safetySettings`). The adapter whose wire family matches
    /// splices the field into its request body; a path that collides with
    /// adapter-owned state is a typed capability error (Section 12).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

impl TurnOptions {
    /// True when the options change nothing relative to the layer below.
    #[must_use]
    pub fn is_default(&self) -> bool {
        self.params.is_default()
            && self.reasoning.is_none()
            && self.tools.is_none()
            && self.extensions.is_empty()
    }

    /// Overlays `self` on `base`: set fields win, unset fields inherit.
    #[must_use]
    pub fn over(&self, base: &Self) -> Self {
        let mut extensions = base.extensions.clone();
        extensions.extend(self.extensions.clone());
        Self {
            params: self.params.over(&base.params),
            reasoning: self.reasoning.or(base.reasoning),
            tools: self.tools.clone().or_else(|| base.tools.clone()),
            extensions,
        }
    }
}

/// Bounds that keep a stream's memory predictable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLimits {
    /// Maximum bytes held for one unfinished part.
    pub max_part_bytes: usize,
    /// Maximum events buffered between the engine and a consumer.
    pub max_buffered_events: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_part_bytes: 8 * 1024 * 1024,
            max_buffered_events: 256,
        }
    }
}

/// Everything an adapter needs to produce one generation. Messages are the
/// whole conversation, in order; the last one is what the model must answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRequest {
    pub model: ModelRef,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub reasoning: ReasoningRequest,
    pub params: GenerationParams,
    pub limits: StreamLimits,
    /// Positions (exclusive end indices into `messages`) at which the provider
    /// may place a prompt-cache breakpoint. Deterministic: computed by
    /// reconstruction and unchanged when a new turn is appended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_boundaries: Vec<u32>,
    pub extensions: BTreeMap<String, Value>,
}

impl ModelRequest {
    #[must_use]
    pub fn new(model: ModelRef, messages: Vec<Message>) -> Self {
        Self {
            model,
            messages,
            tools: Vec::new(),
            reasoning: ReasoningRequest::default(),
            params: GenerationParams::default(),
            limits: StreamLimits::default(),
            cache_boundaries: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }
}

/// Splices host-declared vendor request extras into a wire body (Section 12).
///
/// Extras live in [`ModelRequest::extensions`] under keys of the form
/// `<wire-family>:<field>` (for example `google.gemini:safetySettings`). Only
/// keys whose family equals `family` are applied; the part after the colon is a
/// dotted path applied inside `body`, creating intermediate objects as needed. A
/// path that collides with a field the adapter already set is a typed
/// [`Error::Capability`], so a host can never silently override adapter-owned
/// wire state.
///
/// # Errors
///
/// Returns [`Error::Capability`] on a colliding path or a scalar the path tries
/// to descend through.
pub fn splice_vendor_extensions(
    model: &ModelRef,
    family: &str,
    extensions: &BTreeMap<String, Value>,
    body: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    let prefix = format!("{family}:");
    for (key, value) in extensions {
        let Some(path) = key.strip_prefix(&prefix) else {
            continue;
        };
        let segments: Vec<&str> = path.split('.').collect();
        insert_extension(model, key, &segments, value.clone(), body)?;
    }
    Ok(())
}

fn insert_extension(
    model: &ModelRef,
    key: &str,
    segments: &[&str],
    value: Value,
    body: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    let Some((head, rest)) = segments.split_first() else {
        return Ok(());
    };
    if head.is_empty() {
        return Err(Error::Capability {
            model: model.clone(),
            feature: format!("vendor extension `{key}` has an empty path segment"),
        });
    }
    if rest.is_empty() {
        if body.contains_key(*head) {
            return Err(Error::Capability {
                model: model.clone(),
                feature: format!("vendor extension `{key}` collides with a request field"),
            });
        }
        body.insert((*head).to_owned(), value);
        return Ok(());
    }
    let entry = body
        .entry((*head).to_owned())
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    if let Value::Object(map) = entry {
        insert_extension(model, key, rest, value, map)
    } else {
        Err(Error::Capability {
            model: model.clone(),
            feature: format!("vendor extension `{key}` conflicts with a scalar request field"),
        })
    }
}

/// Adapter-owned interpretation of continuity tokens.
///
/// Core stores and moves tokens; only the adapter that produced them knows how
/// to build, merge and replay them.
pub trait ContinuityAdapter: Send + Sync {
    /// Builds a token from vendor wire state.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the wire value cannot be encoded.
    fn build(&self, wire: &Value) -> Result<ContinuityToken>;

    /// Merges a streamed fragment into the token under construction.
    ///
    /// # Errors
    ///
    /// Returns a codec error when the fragment is malformed.
    fn merge(
        &self,
        base: Option<ContinuityToken>,
        fragment: ContinuityFragment,
    ) -> Result<ContinuityToken>;

    fn replay(
        &self,
        target: &ModelRef,
        history: &[Message],
        token: &ContinuityToken,
    ) -> ReplayDecision;
}

/// The single model port. Streaming is the primitive; a blocking result is the
/// collected stream.
#[async_trait::async_trait]
pub trait Model: Send + Sync {
    fn reference(&self) -> &ModelRef;

    fn capabilities(&self) -> &Capabilities;

    fn continuity(&self) -> &dyn ContinuityAdapter;

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream>;
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use serde_json::json;

    use super::*;

    fn model() -> ModelRef {
        ModelRef::from_str("google/gemini-2.5-pro").expect("model")
    }

    #[test]
    fn vendor_extensions_apply_only_for_the_matching_family() {
        let mut extensions = BTreeMap::new();
        extensions.insert(
            "google.gemini:safetySettings".to_owned(),
            json!([{ "category": "HARM" }]),
        );
        extensions.insert(
            "google.gemini:generationConfig.responseModalities".to_owned(),
            json!(["TEXT"]),
        );
        extensions.insert("openai.responses:metadata".to_owned(), json!({ "k": 1 }));

        let mut body = serde_json::Map::new();
        body.insert("generationConfig".to_owned(), json!({ "temperature": 0.5 }));
        splice_vendor_extensions(&model(), "google.gemini", &extensions, &mut body)
            .expect("splice");

        assert_eq!(body["safetySettings"][0]["category"], "HARM");
        assert_eq!(body["generationConfig"]["responseModalities"][0], "TEXT");
        assert_eq!(body["generationConfig"]["temperature"], json!(0.5));
        assert!(!body.contains_key("metadata"));
    }

    #[test]
    fn a_vendor_extension_may_not_override_a_request_field() {
        let mut extensions = BTreeMap::new();
        extensions.insert("google.gemini:contents".to_owned(), json!([]));

        let mut body = serde_json::Map::new();
        body.insert("contents".to_owned(), json!([{ "role": "user" }]));
        let error = splice_vendor_extensions(&model(), "google.gemini", &extensions, &mut body)
            .expect_err("collision");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn a_vendor_extension_may_not_descend_through_a_scalar() {
        let mut extensions = BTreeMap::new();
        extensions.insert("google.gemini:model.extra".to_owned(), json!(true));

        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), json!("gemini-2.5-pro"));
        let error = splice_vendor_extensions(&model(), "google.gemini", &extensions, &mut body)
            .expect_err("scalar");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn turn_options_overlay_merges_extensions_per_key() {
        let base = TurnOptions {
            extensions: BTreeMap::from([
                ("a".to_owned(), json!(1)),
                ("shared".to_owned(), json!("base")),
            ]),
            ..TurnOptions::default()
        };
        let over = TurnOptions {
            extensions: BTreeMap::from([
                ("b".to_owned(), json!(2)),
                ("shared".to_owned(), json!("over")),
            ]),
            ..TurnOptions::default()
        };
        let merged = over.over(&base);
        assert_eq!(merged.extensions.get("a"), Some(&json!(1)));
        assert_eq!(merged.extensions.get("b"), Some(&json!(2)));
        assert_eq!(merged.extensions.get("shared"), Some(&json!("over")));
    }
}
