use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::domain::media::{MediaKind, MimeType};
use crate::domain::part::Part;
use crate::error::{Error, Result};
use crate::id::ModelRef;
use crate::model::{GenerationParams, MAX_TOP_LOGPROBS};
use crate::tool::ToolSpec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Modality {
    Text,
    Image,
    Audio,
    Video,
    Document,
}

impl From<MediaKind> for Modality {
    fn from(kind: MediaKind) -> Self {
        match kind {
            MediaKind::Image => Modality::Image,
            MediaKind::Audio => Modality::Audio,
            MediaKind::Video => Modality::Video,
            MediaKind::Document => Modality::Document,
        }
    }
}

/// Per-modality limits supplied by the host. Every field is optional and an
/// empty set means "no restriction": the framework enforces only what the host
/// configures, so it never needs to know any model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalityLimits {
    /// Accepted MIME types. Empty means any MIME type is accepted.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub mime_types: BTreeSet<MimeType>,
    /// Maximum inline payload size in bytes, when the bytes travel with the part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<u64>,
    /// Maximum number of parts of this modality per request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_count: Option<u32>,
}

impl ModalityLimits {
    #[must_use]
    pub fn accepts_mime(&self, mime: &MimeType) -> bool {
        self.mime_types.is_empty() || self.mime_types.contains(mime)
    }
}

/// What the model accepts on input and can produce on output. Input modalities
/// carry the host's limits; output modalities are a plain set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalityCapabilities {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub input: BTreeMap<Modality, ModalityLimits>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub output: BTreeSet<Modality>,
}

impl ModalityCapabilities {
    #[must_use]
    pub fn new(
        input: impl IntoIterator<Item = Modality>,
        output: impl IntoIterator<Item = Modality>,
    ) -> Self {
        Self {
            input: input
                .into_iter()
                .map(|m| (m, ModalityLimits::default()))
                .collect(),
            output: output.into_iter().collect(),
        }
    }

    pub fn allow_input(&mut self, modality: Modality, limits: ModalityLimits) {
        self.input.insert(modality, limits);
    }

    pub fn allow_output(&mut self, modality: Modality) {
        self.output.insert(modality);
    }

    #[must_use]
    pub fn supports_input(&self, modality: Modality) -> bool {
        self.input.contains_key(&modality)
    }

    #[must_use]
    pub fn supports_output(&self, modality: Modality) -> bool {
        self.output.contains(&modality)
    }

    #[must_use]
    pub fn limits(&self, modality: Modality) -> Option<&ModalityLimits> {
        self.input.get(&modality)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
}

/// A reasoning request in neutral terms. The host resolves it against the
/// per-model capability data; an unsupported request is a typed error, never a
/// silent downgrade.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "value")]
#[non_exhaustive]
pub enum ReasoningRequest {
    Disabled,
    #[default]
    Automatic,
    Effort(ReasoningEffort),
    Budget(u32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reasoning {
    pub supported: bool,
    pub can_disable: bool,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub efforts: BTreeSet<ReasoningEffort>,
    pub supports_budget: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolKind {
    Function,
    ProviderExecuted,
    ProviderSchema,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StreamingGranularity {
    Delta,
    Part,
    Disabled,
}

/// Whether and how the model caches prompt prefixes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CacheMode {
    #[default]
    None,
    Automatic,
    Explicit,
}

/// Independent on/off capabilities. They are a closed, typed set instead of
/// loose booleans so hosts and adapters stay in sync as providers evolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Feature {
    Citations,
    ParallelToolCalls,
    ForcedToolChoice,
    Batch,
    /// Token-level log probabilities (`top_logprobs`) for a turn.
    Logprobs,
}

/// The kinds of structured output a model can produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StructuredOutputMode {
    JsonObject,
    JsonSchema,
    StrictSchema,
}

/// What a model can do. The host fills this in from whatever source it trusts
/// (a JSON file, an endpoint, provider docs); the engine only asks.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Capabilities {
    pub modalities: ModalityCapabilities,
    #[serde(default)]
    pub reasoning: Reasoning,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tool_kinds: BTreeSet<ToolKind>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub features: BTreeSet<Feature>,
    pub streaming: Option<StreamingGranularity>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub structured_output: BTreeSet<StructuredOutputMode>,
    pub cache: CacheMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
}

impl Capabilities {
    /// Starts a fluent, fully typed builder.
    ///
    /// Every setter takes a closed enum or a typed value, never a string, so a
    /// descriptor is built by name instead of by string or field position.
    #[must_use]
    pub fn builder() -> CapabilitiesBuilder {
        CapabilitiesBuilder::default()
    }

    #[must_use]
    pub fn supports(&self, kind: ToolKind) -> bool {
        self.tool_kinds.contains(&kind)
    }

    #[must_use]
    pub fn supports_feature(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Fails unless the model supports the requested reasoning mode.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capability`] when the request cannot be honored.
    pub fn check_reasoning(&self, model: &ModelRef, request: ReasoningRequest) -> Result<()> {
        let reasoning = &self.reasoning;
        match request {
            ReasoningRequest::Disabled => {
                if reasoning.supported && !reasoning.can_disable {
                    return Err(Error::Capability {
                        model: model.clone(),
                        feature: "disabling reasoning".to_owned(),
                    });
                }
            }
            ReasoningRequest::Automatic => {}
            ReasoningRequest::Effort(effort) => {
                if !reasoning.supported || !reasoning.efforts.contains(&effort) {
                    return Err(Error::Capability {
                        model: model.clone(),
                        feature: format!("reasoning effort {effort:?}"),
                    });
                }
            }
            ReasoningRequest::Budget(budget) => {
                if !reasoning.supported || !reasoning.supports_budget {
                    return Err(Error::Capability {
                        model: model.clone(),
                        feature: "a reasoning token budget".to_owned(),
                    });
                }
                if reasoning.max_budget.is_some_and(|max| budget > max) {
                    return Err(Error::Capability {
                        model: model.clone(),
                        feature: format!(
                            "a reasoning budget above {}",
                            reasoning.max_budget.unwrap_or(0)
                        ),
                    });
                }
            }
        }
        Ok(())
    }

    /// Enforces the host-configured limits for every part. Without configured
    /// limits nothing is rejected, which is the model-agnostic default.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capability`] for an unsupported modality, an unaccepted
    /// MIME type, an oversized payload, or too many parts of one modality.
    pub fn check_parts(&self, model: &ModelRef, parts: &[Part]) -> Result<()> {
        let mut counts: BTreeMap<Modality, u32> = BTreeMap::new();
        for part in parts {
            let Part::Media(media) = part else {
                continue;
            };
            let modality = Modality::from(media.kind);
            let Some(limits) = self.modalities.limits(modality) else {
                return Err(Error::Capability {
                    model: model.clone(),
                    feature: format!("input modality {modality:?}"),
                });
            };
            if !limits.accepts_mime(&media.mime) {
                return Err(Error::Capability {
                    model: model.clone(),
                    feature: format!("MIME type {}", media.mime),
                });
            }
            if let Some(max_bytes) = limits.max_bytes {
                let bytes = media.inline_len() as u64;
                if bytes > max_bytes {
                    return Err(Error::Capability {
                        model: model.clone(),
                        feature: format!("{modality:?} larger than {max_bytes} bytes"),
                    });
                }
            }
            let count = counts.entry(modality).or_insert(0);
            *count += 1;
            if limits.max_count.is_some_and(|max| *count > max) {
                return Err(Error::Capability {
                    model: model.clone(),
                    feature: format!("more than {count} {modality:?} parts"),
                });
            }
        }
        Ok(())
    }

    /// Fails when the model cannot accept one of the offered tool kinds.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capability`] for an unsupported tool kind.
    pub fn check_tools(&self, model: &ModelRef, tools: &[ToolSpec]) -> Result<()> {
        for tool in tools {
            if !self.supports(tool.kind) {
                return Err(Error::Capability {
                    model: model.clone(),
                    feature: format!("tool kind {:?} ({})", tool.kind, tool.name),
                });
            }
        }
        Ok(())
    }

    /// Fails when the model cannot honor a generation parameter the turn asks
    /// for. Only `logprobs` is gated today; the other parameters are either
    /// wire-level no-ops or covered by the modality, tool and reasoning checks
    /// (FR-LP-1).
    ///
    /// # Errors
    ///
    /// Returns [`Error::Capability`] when the model does not declare
    /// [`Feature::Logprobs`], and [`Error::LimitExceeded`] above
    /// [`MAX_TOP_LOGPROBS`].
    pub fn check_generation(&self, model: &ModelRef, params: &GenerationParams) -> Result<()> {
        let Some(top) = params.logprobs else {
            return Ok(());
        };
        if top > MAX_TOP_LOGPROBS {
            return Err(Error::LimitExceeded {
                what: "logprobs".to_owned(),
                limit: usize::from(MAX_TOP_LOGPROBS),
            });
        }
        if !self.supports_feature(Feature::Logprobs) {
            return Err(Error::Capability {
                model: model.clone(),
                feature: "log probabilities".to_owned(),
            });
        }
        Ok(())
    }
}

/// Fluent, fully typed constructor for [`Capabilities`], started by
/// [`Capabilities::builder`].
#[derive(Debug, Default)]
pub struct CapabilitiesBuilder {
    capabilities: Capabilities,
}

impl CapabilitiesBuilder {
    /// Accepts `modality` as an input modality with no extra limits.
    #[must_use]
    pub fn input(mut self, modality: Modality) -> Self {
        self.capabilities
            .modalities
            .allow_input(modality, ModalityLimits::default());
        self
    }

    /// Accepts `modality` as an input modality under host-supplied limits.
    #[must_use]
    pub fn input_limits(mut self, modality: Modality, limits: ModalityLimits) -> Self {
        self.capabilities.modalities.allow_input(modality, limits);
        self
    }

    /// Produces `modality` as an output modality.
    #[must_use]
    pub fn output(mut self, modality: Modality) -> Self {
        self.capabilities.modalities.allow_output(modality);
        self
    }

    /// Sets the reasoning descriptor.
    #[must_use]
    pub fn reasoning(mut self, reasoning: Reasoning) -> Self {
        self.capabilities.reasoning = reasoning;
        self
    }

    /// Adds one accepted tool kind.
    #[must_use]
    pub fn tool_kind(mut self, kind: ToolKind) -> Self {
        self.capabilities.tool_kinds.insert(kind);
        self
    }

    /// Adds several accepted tool kinds.
    #[must_use]
    pub fn tool_kinds(mut self, kinds: impl IntoIterator<Item = ToolKind>) -> Self {
        self.capabilities.tool_kinds.extend(kinds);
        self
    }

    /// Adds one supported feature flag.
    #[must_use]
    pub fn feature(mut self, feature: Feature) -> Self {
        self.capabilities.features.insert(feature);
        self
    }

    /// Adds several supported feature flags.
    #[must_use]
    pub fn features(mut self, features: impl IntoIterator<Item = Feature>) -> Self {
        self.capabilities.features.extend(features);
        self
    }

    /// Sets the streaming granularity.
    #[must_use]
    pub fn streaming(mut self, granularity: StreamingGranularity) -> Self {
        self.capabilities.streaming = Some(granularity);
        self
    }

    /// Adds one supported structured-output mode.
    #[must_use]
    pub fn structured_output(mut self, mode: StructuredOutputMode) -> Self {
        self.capabilities.structured_output.insert(mode);
        self
    }

    /// Sets the prompt-cache mode.
    #[must_use]
    pub fn cache(mut self, mode: CacheMode) -> Self {
        self.capabilities.cache = mode;
        self
    }

    /// Sets the maximum context window in tokens.
    #[must_use]
    pub fn max_context(mut self, tokens: u64) -> Self {
        self.capabilities.max_context = Some(tokens);
        self
    }

    /// Sets the maximum output in tokens.
    #[must_use]
    pub fn max_output(mut self, tokens: u64) -> Self {
        self.capabilities.max_output = Some(tokens);
        self
    }

    /// Finishes the descriptor.
    #[must_use]
    pub fn build(self) -> Capabilities {
        self.capabilities
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn model() -> ModelRef {
        ModelRef::from_str("openai/gpt-5").expect("model")
    }

    #[test]
    fn reasoning_errors_name_the_model_and_feature() {
        let capabilities = Capabilities::default();
        let error = capabilities
            .check_reasoning(&model(), ReasoningRequest::Effort(ReasoningEffort::High))
            .expect_err("unsupported");
        match error {
            Error::Capability {
                model: named,
                feature,
            } => {
                assert_eq!(named, model());
                assert!(feature.contains("reasoning"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn media_needs_the_input_modality() {
        use crate::domain::media::{MediaPart, MediaSource};
        let part = Part::Media(MediaPart::new(
            MediaKind::Image,
            MediaSource::Url("https://example.com/a.png".to_owned()),
            MimeType::new("image/png").expect("mime"),
        ));
        assert!(
            Capabilities::default()
                .check_parts(&model(), &[part])
                .is_err()
        );
    }

    #[test]
    fn configured_limits_are_enforced_and_optional() {
        use crate::domain::media::{MediaPart, MediaSource};
        let mut capabilities = Capabilities::default();
        let mut limits = ModalityLimits::default();
        limits
            .mime_types
            .insert(MimeType::new("image/png").expect("mime"));
        limits.max_count = Some(1);
        capabilities.modalities.allow_input(Modality::Image, limits);

        let png = || {
            Part::Media(MediaPart::new(
                MediaKind::Image,
                MediaSource::Inline(vec![0u8; 4]),
                MimeType::new("image/png").expect("mime"),
            ))
        };
        assert!(capabilities.check_parts(&model(), &[png()]).is_ok());
        assert!(capabilities.check_parts(&model(), &[png(), png()]).is_err());

        let webp = Part::Media(MediaPart::new(
            MediaKind::Image,
            MediaSource::Inline(vec![0u8; 4]),
            MimeType::new("image/webp").expect("mime"),
        ));
        assert!(capabilities.check_parts(&model(), &[webp]).is_err());
    }

    #[test]
    fn the_builder_composes_a_descriptor_without_strings() {
        let capabilities = Capabilities::builder()
            .input(Modality::Text)
            .input(Modality::Image)
            .output(Modality::Text)
            .tool_kind(ToolKind::Function)
            .feature(Feature::ParallelToolCalls)
            .streaming(StreamingGranularity::Delta)
            .reasoning(Reasoning {
                supported: true,
                supports_budget: true,
                ..Reasoning::default()
            })
            .max_context(200_000)
            .max_output(32_000)
            .build();

        assert!(capabilities.modalities.supports_input(Modality::Text));
        assert!(capabilities.modalities.supports_input(Modality::Image));
        assert!(capabilities.modalities.supports_output(Modality::Text));
        assert!(!capabilities.modalities.supports_output(Modality::Image));
        assert!(capabilities.supports(ToolKind::Function));
        assert!(capabilities.supports_feature(Feature::ParallelToolCalls));
        assert_eq!(capabilities.streaming, Some(StreamingGranularity::Delta));
        assert!(capabilities.reasoning.supported);
        assert_eq!(capabilities.max_context, Some(200_000));
        assert_eq!(capabilities.max_output, Some(32_000));
    }
}
