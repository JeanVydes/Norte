use std::collections::BTreeMap;
use std::pin::Pin;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::capability::{Capabilities, ReasoningRequest};
use crate::domain::continuity::{ContinuityFragment, ContinuityToken, ReplayDecision};
use crate::domain::message::Message;
use crate::error::{Error, Result};
use crate::id::{ModelRef, ToolName, VendorExtensionKey, WireFamily};
use crate::stream::StreamEvent;
use crate::tool::ToolSpec;

/// The boxed event stream one model call returns.
pub type ModelStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send + 'static>>;

/// Request extension key carrying the originating session id, so adapters can
/// derive a stable provider cache key without inventing their own identity.
pub const SESSION_EXTENSION: VendorExtensionKey = VendorExtensionKey::Session;

/// Session-metadata key carrying the effective request options of the last turn
/// (FR-EXP-7). This is a convention for hosts that persist turn options;
/// the model port does not manage session metadata.
pub const TURN_OPTIONS_KEY: &str = "norte.turn_options";

/// Session-metadata key carrying the tool results already produced for an
/// assistant message whose remaining calls parked for approval, so resuming
/// does not run those calls a second time (FR-APPR-8). This legacy host
/// convention is not read or maintained by the model port.
pub const PARTIAL_TOOLS_KEY: &str = "norte.partial_tools";

/// The largest `top_logprobs` count the neutral model accepts. Every supported
/// wire family caps the count at 20.
pub const MAX_TOP_LOGPROBS: u8 = 20;

/// Sampling and output controls shared by every adapter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GenerationParams {
    /// Sampling temperature: lower values repeat more, higher values vary more.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Nucleus-sampling mass, in `(0, 1]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Keeps only the `n` most probable tokens at each step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Discards the least likely tokens before sampling, as a mass in `(0, 1]`.
    ///
    /// A vendor knob (`OpenAI`-compatible gateways) with no native `OpenAI`
    /// equivalent; the adapter emits it only because a host that sets it has
    /// chosen a vendor that accepts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f32>,
    /// Truncates the tail by its ratio to the most likely token (`top-a`).
    ///
    /// `OpenRouter`-flavoured: native `OpenAI` rejects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_a: Option<f32>,
    /// Penalises tokens already present in the answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f32>,
    /// Penalises tokens already present in the prompt or answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f32>,
    /// Penalises repeated tokens, across both streams.
    ///
    /// A vendor knob (`OpenRouter` and compatible gateways); native `OpenAI` has
    /// `frequency_penalty`/`presence_penalty` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repetition_penalty: Option<f32>,
    /// Per-token bias applied before sampling, keyed by token id as a string.
    ///
    /// The `OpenAI` shape (`{"50256": -100}`); `BTreeMap` so the wire is stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logit_bias: Option<BTreeMap<String, f32>>,
    /// Cap on the tokens one answer may produce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// Sequences that end generation; the sequence is not part of the answer.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    /// Sampling seed, where the vendor honours it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// JSON Schema the output must satisfy, when the model supports structured
    /// output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Token-level log probabilities for this turn: `Some(0)` reports only the
    /// chosen token's probability, `Some(n)` adds up to `n` runner-up tokens,
    /// and `None` keeps probabilities off the wire entirely. The engine rejects
    /// a count above [`MAX_TOP_LOGPROBS`], and rejects any count when the model
    /// does not declare [`Feature::Logprobs`](crate::Feature::Logprobs), before
    /// the request is built (FR-LP-1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<u8>,
}

impl GenerationParams {
    /// Lists the neutral parameter values present in this request.
    pub fn requested_params(&self) -> impl Iterator<Item = norte_types::SupportedParam> {
        use norte_types::SupportedParam;
        [
            (SupportedParam::Temperature, self.temperature.is_some()),
            (SupportedParam::TopP, self.top_p.is_some()),
            (SupportedParam::TopK, self.top_k.is_some()),
            (SupportedParam::MinP, self.min_p.is_some()),
            (SupportedParam::TopA, self.top_a.is_some()),
            (
                SupportedParam::FrequencyPenalty,
                self.frequency_penalty.is_some(),
            ),
            (
                SupportedParam::PresencePenalty,
                self.presence_penalty.is_some(),
            ),
            (
                SupportedParam::RepetitionPenalty,
                self.repetition_penalty.is_some(),
            ),
            (SupportedParam::LogitBias, self.logit_bias.is_some()),
            (
                SupportedParam::MaxOutputTokens,
                self.max_output_tokens.is_some(),
            ),
            (
                SupportedParam::StopSequences,
                !self.stop_sequences.is_empty(),
            ),
            (SupportedParam::Seed, self.seed.is_some()),
        ]
        .into_iter()
        .filter_map(|(param, present)| present.then_some(param))
    }

    /// Checks values that are invalid independently of a provider's supported
    /// parameter ranges.
    ///
    /// Provider-specific ranges remain the adapter's responsibility; this
    /// method rejects non-finite numbers and universally invalid sampling
    /// bounds before values reach JSON serialization.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] for a non-finite number or an invalid
    /// probability/count bound.
    pub fn validate(&self) -> Result<()> {
        let finite = [
            ("temperature", self.temperature),
            ("top_p", self.top_p),
            ("min_p", self.min_p),
            ("top_a", self.top_a),
            ("frequency_penalty", self.frequency_penalty),
            ("presence_penalty", self.presence_penalty),
            ("repetition_penalty", self.repetition_penalty),
        ];
        for (name, value) in finite {
            if value.is_some_and(|value| !value.is_finite()) {
                return Err(Error::InvalidInput(format!(
                    "generation parameter `{name}` must be finite"
                )));
            }
        }
        for (name, value) in [("top_p", self.top_p), ("min_p", self.min_p)] {
            if value.is_some_and(|value| !(0.0..=1.0).contains(&value) || value == 0.0) {
                return Err(Error::InvalidInput(format!(
                    "generation parameter `{name}` must be in (0, 1]"
                )));
            }
        }
        if self.temperature.is_some_and(|value| value < 0.0) {
            return Err(Error::InvalidInput(
                "generation parameter `temperature` must be non-negative".to_owned(),
            ));
        }
        if self.top_k == Some(0) {
            return Err(Error::InvalidInput(
                "generation parameter `top_k` must be greater than zero".to_owned(),
            ));
        }
        if self
            .logit_bias
            .as_ref()
            .is_some_and(|bias| bias.values().any(|value| !value.is_finite()))
        {
            return Err(Error::InvalidInput(
                "generation parameter `logit_bias` values must be finite".to_owned(),
            ));
        }
        Ok(())
    }

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
            min_p: self.min_p.or(base.min_p),
            top_a: self.top_a.or(base.top_a),
            frequency_penalty: self.frequency_penalty.or(base.frequency_penalty),
            presence_penalty: self.presence_penalty.or(base.presence_penalty),
            repetition_penalty: self.repetition_penalty.or(base.repetition_penalty),
            logit_bias: self.logit_bias.clone().or_else(|| base.logit_bias.clone()),
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
            logprobs: self.logprobs.or(base.logprobs),
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
    pub extensions: BTreeMap<VendorExtensionKey, Value>,
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

/// The bound applied by [`crate::StreamAccumulator`] to one unfinished part.
///
/// Event-channel capacity is owned by the consumer because the model port does
/// not create a channel. Adapters and transports may enforce additional wire
/// size limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamLimits {
    /// Maximum bytes held for one unfinished part.
    pub max_part_bytes: usize,
}

impl Default for StreamLimits {
    fn default() -> Self {
        Self {
            max_part_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Everything an adapter needs to produce one generation. Messages are the
/// whole conversation, in order; the last one is what the model must answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelRequest {
    /// The exact model that must answer.
    pub model: ModelRef,
    /// The whole conversation, in order; the last message is what must be
    /// answered.
    pub messages: Vec<Message>,
    /// The tools the model may call, already resolved from the turn's subset.
    pub tools: Vec<ToolSpec>,
    /// The reasoning ask for this request.
    pub reasoning: ReasoningRequest,
    /// The effective generation parameters (FR-ENG-9).
    pub params: GenerationParams,
    /// Limits applied by Norte's stream accumulator. These do not configure
    /// transport frame sizes or host event-channel capacity.
    pub limits: StreamLimits,
    /// Positions (exclusive end indices into `messages`) at which the provider
    /// may place a prompt-cache breakpoint. Deterministic: computed by
    /// reconstruction and unchanged when a new turn is appended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cache_boundaries: Vec<u32>,
    /// Vendor request extras, keyed `<wire-family>:<field>`; the adapter whose
    /// family matches splices them into its body (Section 12).
    pub extensions: BTreeMap<VendorExtensionKey, Value>,
}

impl ModelRequest {
    /// A request for `model` carrying `messages`, everything else at its
    /// default.
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
    extensions: &BTreeMap<VendorExtensionKey, Value>,
    body: &mut serde_json::Map<String, Value>,
) -> Result<()> {
    for (key, value) in extensions {
        if key.family().map(WireFamily::as_str) != Some(family) {
            continue;
        }
        let Some(path) = key.path() else {
            continue;
        };
        let segments: Vec<&str> = path.split('.').collect();
        insert_extension(model, key, &segments, value.clone(), body)?;
    }
    Ok(())
}

fn insert_extension(
    model: &ModelRef,
    key: &VendorExtensionKey,
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

    /// Decides what to do with `token` when replaying `history` into `target`.
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
    /// The exact model this port answers for, as the host named it.
    fn reference(&self) -> &ModelRef;

    /// What this model declares it can do.
    fn capabilities(&self) -> &Capabilities;

    /// The adapter that interprets this model's continuity tokens.
    fn continuity(&self) -> &dyn ContinuityAdapter;

    /// Runs one generation and returns its events as they arrive.
    ///
    /// A successful stream includes [`StreamEvent::Finish`]. EOF without it is
    /// incomplete, not an implicit successful stop. Usage may follow `Finish`.
    /// An error terminates the stream. Hosts cancel an opening call by dropping
    /// this future, or an active call by dropping the returned stream; tools
    /// have a separate cooperative cancellation contract.
    ///
    /// # Errors
    ///
    /// Returns a transport or wire error when the request cannot be run.
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
            "google.gemini:safetySettings"
                .parse()
                .expect("extension key"),
            json!([{ "category": "HARM" }]),
        );
        extensions.insert(
            "google.gemini:generationConfig.responseModalities"
                .parse()
                .expect("extension key"),
            json!(["TEXT"]),
        );
        extensions.insert(
            "openai.responses:metadata".parse().expect("extension key"),
            json!({ "k": 1 }),
        );

        let mut body = serde_json::Map::new();
        body.insert("generationConfig".to_owned(), json!({ "temperature": 0.5 }));
        let family = WireFamily::new("google.gemini").expect("wire family");
        splice_vendor_extensions(&model(), family.as_str(), &extensions, &mut body)
            .expect("splice");

        assert_eq!(body["safetySettings"][0]["category"], "HARM");
        assert_eq!(body["generationConfig"]["responseModalities"][0], "TEXT");
        assert_eq!(body["generationConfig"]["temperature"], json!(0.5));
        assert!(!body.contains_key("metadata"));
    }

    #[test]
    fn a_vendor_extension_may_not_override_a_request_field() {
        let mut extensions = BTreeMap::new();
        extensions.insert(
            "google.gemini:contents".parse().expect("extension key"),
            json!([]),
        );

        let mut body = serde_json::Map::new();
        body.insert("contents".to_owned(), json!([{ "role": "user" }]));
        let family = WireFamily::new("google.gemini").expect("wire family");
        let error = splice_vendor_extensions(&model(), family.as_str(), &extensions, &mut body)
            .expect_err("collision");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn a_vendor_extension_may_not_descend_through_a_scalar() {
        let mut extensions = BTreeMap::new();
        extensions.insert(
            "google.gemini:model.extra".parse().expect("extension key"),
            json!(true),
        );

        let mut body = serde_json::Map::new();
        body.insert("model".to_owned(), json!("gemini-2.5-pro"));
        let family = WireFamily::new("google.gemini").expect("wire family");
        let error = splice_vendor_extensions(&model(), family.as_str(), &extensions, &mut body)
            .expect_err("scalar");
        assert!(matches!(error, Error::Capability { .. }));
    }

    #[test]
    fn turn_options_overlay_merges_extensions_per_key() {
        let base = TurnOptions {
            extensions: BTreeMap::from([
                ("google.gemini:a".parse().expect("extension key"), json!(1)),
                (
                    "google.gemini:shared".parse().expect("extension key"),
                    json!("base"),
                ),
            ]),
            ..TurnOptions::default()
        };
        let over = TurnOptions {
            extensions: BTreeMap::from([
                ("google.gemini:b".parse().expect("extension key"), json!(2)),
                (
                    "google.gemini:shared".parse().expect("extension key"),
                    json!("over"),
                ),
            ]),
            ..TurnOptions::default()
        };
        let merged = over.over(&base);
        assert_eq!(
            merged
                .extensions
                .get(&"google.gemini:a".parse().expect("key")),
            Some(&json!(1))
        );
        assert_eq!(
            merged
                .extensions
                .get(&"google.gemini:b".parse().expect("key")),
            Some(&json!(2))
        );
        assert_eq!(
            merged
                .extensions
                .get(&"google.gemini:shared".parse().expect("key")),
            Some(&json!("over"))
        );
    }

    #[test]
    fn extension_keys_round_trip_as_map_keys_and_validate_paths() {
        let extensions = BTreeMap::from([
            (VendorExtensionKey::Session, json!("session-1")),
            (
                "google.gemini:generationConfig.responseModalities"
                    .parse()
                    .expect("vendor extension key"),
                json!(["TEXT"]),
            ),
        ]);
        let encoded = serde_json::to_string(&extensions).expect("serialize map");
        let decoded: BTreeMap<VendorExtensionKey, Value> =
            serde_json::from_str(&encoded).expect("deserialize map");
        assert_eq!(decoded, extensions);
        assert!(
            "google.gemini:generationConfig..temperature"
                .parse::<VendorExtensionKey>()
                .is_err()
        );
    }

    #[test]
    fn generation_params_reject_non_finite_and_invalid_bounds() {
        assert!(
            GenerationParams {
                temperature: Some(f32::NAN),
                ..GenerationParams::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            GenerationParams {
                top_p: Some(0.0),
                ..GenerationParams::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            GenerationParams {
                top_k: Some(0),
                ..GenerationParams::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            GenerationParams {
                top_p: Some(0.9),
                temperature: Some(1.0),
                ..GenerationParams::default()
            }
            .validate()
            .is_ok()
        );
    }
}
