use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::domain::continuity::ContinuityToken;
use crate::domain::media::{MediaPart, MediaSource};
use crate::id::{BlobId, CallId, ProviderId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReasoningVisibility {
    Full,
    Summary,
    Redacted,
    Omitted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum CitationSource {
    Url(String),
    Blob(BlobId),
    ProviderFile {
        provider: ProviderId,
        file_id: String,
    },
    Range {
        start: u32,
        end: u32,
    },
    /// A vendor citation the neutral model does not structure; the payload is the
    /// vendor's own citation object, kept so attribution survives without a
    /// lossy guess.
    Extension {
        namespace: String,
        data: Value,
    },
}

/// A provider-neutral reference attached to text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    pub source: CitationSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextPart {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

impl TextPart {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            citations: Vec::new(),
            continuity: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReasoningPart {
    pub visibility: ReasoningVisibility,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallPart {
    pub call_id: CallId,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResultPart {
    pub call_id: CallId,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    #[serde(default)]
    pub is_error: bool,
}

impl ToolResultPart {
    #[must_use]
    pub fn text(call_id: CallId, text: impl Into<String>) -> Self {
        Self {
            call_id,
            parts: vec![Part::Text(TextPart::new(text))],
            structured: None,
            is_error: false,
        }
    }

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
    pub namespace: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolCallPart {
    pub call_id: CallId,
    pub tool: ProviderTool,
    pub input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderToolResultPart {
    pub call_id: CallId,
    pub tool: ProviderTool,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub citations: Vec<Citation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuity: Option<ContinuityToken>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefusalPart {
    pub text: String,
}

/// Forward compatibility: an unknown wire block round-trips unchanged to the
/// vendor that produced it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtensionPart {
    pub namespace: String,
    pub data: Value,
}

/// One element of a message's ordered content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
#[non_exhaustive]
pub enum Part {
    Text(TextPart),
    Reasoning(ReasoningPart),
    Media(MediaPart),
    ToolCall(ToolCallPart),
    ToolResult(ToolResultPart),
    ProviderToolCall(ProviderToolCallPart),
    ProviderToolResult(ProviderToolResultPart),
    Refusal(RefusalPart),
    Extension(ExtensionPart),
}

impl Part {
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextPart::new(text))
    }

    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Part::Text(text) => Some(&text.text),
            Part::Reasoning(reasoning) => Some(&reasoning.text),
            Part::Refusal(refusal) => Some(&refusal.text),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_tool_call(&self) -> Option<&ToolCallPart> {
        match self {
            Part::ToolCall(call) => Some(call),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_tool_result(&self) -> Option<&ToolResultPart> {
        match self {
            Part::ToolResult(result) => Some(result),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_media(&self) -> Option<&MediaPart> {
        match self {
            Part::Media(media) => Some(media),
            _ => None,
        }
    }

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
