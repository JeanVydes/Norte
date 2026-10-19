use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::continuity::ContinuityToken;
use crate::domain::media::{MediaPart, MediaSource};
use crate::id::{BlobId, CallId, ProviderId};

/// How much of a model's reasoning the provider made available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningVisibility {
    /// The full reasoning text.
    Full,
    /// A provider-written summary of it.
    Summary,
    /// The model reasoned, but the provider returned the reasoning redacted.
    Redacted,
    /// The provider returned no reasoning for this part.
    Omitted,
}

/// Where a citation's source lives.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CitationSource {
    /// A URL.
    Url(String),
    /// Bytes held in the blob store.
    Blob(BlobId),
    /// A file the provider holds.
    ProviderFile {
        /// The provider that holds the file.
        provider: ProviderId,
        /// The provider's id for the file.
        file_id: String,
    },
    /// A character range in the cited document, `start` included and `end`
    /// excluded.
    Range {
        /// The first character of the range.
        start: u32,
        /// One past the last character of the range.
        end: u32,
    },
    /// A vendor citation the neutral model does not structure; the payload is the
    /// vendor's own citation object, kept so attribution survives without a
    /// lossy guess.
    Extension {
        /// The vendor's namespace.
        namespace: String,
        /// The vendor's citation object, kept verbatim.
        data: Value,
    },
}

/// A provider-neutral reference attached to text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// Where the citation points.
    pub source: CitationSource,
    /// A display title, where the provider gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A run of text in a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPart {
    /// The text.
    pub text: String,
    /// References the provider attached to the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The opaque material the adapter needs to replay this text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

impl TextPart {
    /// A text part with no citations and no continuity material.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            citations: Vec::new(),
            continuity: None,
        }
    }
}

/// Reasoning the model produced, at the visibility the provider allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningPart {
    /// How much of the reasoning the provider made available.
    pub visibility: ReasoningVisibility,
    /// The reasoning text.
    pub text: String,
    /// The opaque material the adapter needs to replay this reasoning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

/// A call the model asked the host to make.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallPart {
    /// The call.
    pub call_id: CallId,
    /// The tool's name.
    pub name: String,
    /// The arguments document the model produced.
    pub arguments: Value,
    /// The opaque material the adapter needs to replay this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

/// The result of a tool call, as the host produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPart {
    /// The call this is the result of.
    pub call_id: CallId,
    /// The result's content.
    pub parts: Vec<Part>,
    /// The result's structured value, where it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// Whether the tool reported an error.
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResultPart {
    /// A result carrying one text part.
    #[must_use]
    pub fn text(call_id: CallId, text: impl Into<String>) -> Self {
        Self {
            call_id,
            parts: vec![Part::Text(TextPart::new(text))],
            structured: None,
            is_error: false,
        }
    }

    /// An error result carrying one text part.
    #[must_use]
    pub fn error(call_id: CallId, text: impl Into<String>) -> Self {
        Self {
            is_error: true,
            ..Self::text(call_id, text)
        }
    }
}

/// Identity of a vendor-executed tool. The namespace keeps same-named tools
/// from different vendors or generations apart.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProviderTool {
    /// The vendor's namespace.
    pub namespace: String,
    /// The vendor's name for the tool.
    pub name: String,
}

/// A call to a tool the provider executed itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolCallPart {
    /// The call.
    pub call_id: CallId,
    /// The tool that ran.
    pub tool: ProviderTool,
    /// The input the vendor sent, kept verbatim.
    pub input: Value,
    /// The opaque material the adapter needs to replay this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

/// The result of a tool the provider executed itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolResultPart {
    /// The call this is the result of.
    pub call_id: CallId,
    /// The tool that ran.
    pub tool: ProviderTool,
    /// The output the vendor produced, kept verbatim.
    pub output: Value,
    /// References the provider attached to the result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    /// The opaque material the adapter needs to replay this result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

/// A refusal the model produced.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalPart {
    /// The refusal text.
    pub text: String,
}

/// Forward compatibility: an unknown wire block round-trips unchanged to the
/// vendor that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPart {
    /// The vendor's namespace.
    pub namespace: String,
    /// The block, kept verbatim.
    pub data: Value,
}

/// One element of a message's ordered content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
#[non_exhaustive]
pub enum Part {
    /// Text the model produced or the host supplied.
    Text(TextPart),
    /// Reasoning the model produced.
    Reasoning(ReasoningPart),
    /// Media bytes or a reference to them.
    Media(MediaPart),
    /// A call the model asked the host to make.
    ToolCall(ToolCallPart),
    /// The result of a host tool call.
    ToolResult(ToolResultPart),
    /// A call to a tool the provider executed itself.
    ProviderToolCall(ProviderToolCallPart),
    /// The result of a tool the provider executed itself.
    ProviderToolResult(ProviderToolResultPart),
    /// A refusal the model produced.
    Refusal(RefusalPart),
    /// A block the neutral model does not structure.
    Extension(ExtensionPart),
}

impl Part {
    /// A text part.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextPart::new(text))
    }

    /// The text of a text, reasoning or refusal part.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Part::Text(text) => Some(&text.text),
            Part::Reasoning(reasoning) => Some(&reasoning.text),
            Part::Refusal(refusal) => Some(&refusal.text),
            _ => None,
        }
    }

    /// The part, when it is a call to a host tool.
    #[must_use]
    pub fn as_tool_call(&self) -> Option<&ToolCallPart> {
        match self {
            Part::ToolCall(call) => Some(call),
            _ => None,
        }
    }

    /// The part, when it is the result of a host tool call.
    #[must_use]
    pub fn as_tool_result(&self) -> Option<&ToolResultPart> {
        match self {
            Part::ToolResult(result) => Some(result),
            _ => None,
        }
    }

    /// The part, when it is media.
    #[must_use]
    pub fn as_media(&self) -> Option<&MediaPart> {
        match self {
            Part::Media(media) => Some(media),
            _ => None,
        }
    }

    /// The continuity token the part carries, when it carries one.
    #[must_use]
    pub fn continuity(&self) -> Option<&ContinuityToken> {
        match self {
            Part::Text(part) => part.continuity.as_ref(),
            Part::Reasoning(part) => part.continuity.as_ref(),
            Part::ToolCall(part) => part.continuity.as_ref(),
            Part::ProviderToolCall(part) => part.continuity.as_ref(),
            Part::ProviderToolResult(part) => part.continuity.as_ref(),
            Part::Media(_) | Part::ToolResult(_) | Part::Refusal(_) | Part::Extension(_) => None,
        }
    }

    /// Whether this part carries media bytes that must be checked against the
    /// inline threshold.
    #[must_use]
    pub fn inline_media_len(&self) -> usize {
        match self {
            Part::Media(media) => media.inline_len(),
            _ => 0,
        }
    }

    /// The same part with its continuity token removed, used when an adapter
    /// decides the token cannot be replayed.
    #[must_use]
    pub fn without_continuity(self) -> Self {
        match self {
            Part::Text(mut part) => {
                part.continuity = None;
                Part::Text(part)
            }
            Part::Reasoning(mut part) => {
                part.continuity = None;
                Part::Reasoning(part)
            }
            Part::ToolCall(mut part) => {
                part.continuity = None;
                Part::ToolCall(part)
            }
            Part::ProviderToolCall(mut part) => {
                part.continuity = None;
                Part::ProviderToolCall(part)
            }
            Part::ProviderToolResult(mut part) => {
                part.continuity = None;
                Part::ProviderToolResult(part)
            }
            other => other,
        }
    }

    /// Maps every inline media source through `f`, used when the engine
    /// externalizes bytes above the inline threshold.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `f`.
    pub fn map_inline_media(
        self,
        mut f: impl FnMut(MediaPart) -> Result<MediaPart, crate::Error>,
    ) -> Result<Self, crate::Error> {
        match self {
            Part::Media(media) if matches!(media.source, MediaSource::Inline(_)) => {
                f(media).map(Part::Media)
            }
            other => Ok(other),
        }
    }
}
