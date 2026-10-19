use std::fmt::{Display, Formatter};
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codec;
use crate::error::{Error, Result};
use crate::id::{BlobId, ProviderId};

/// The four media part families the domain models explicitly. Anything else is
/// expected to travel as an extension part.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MediaKind {
    Image,
    Audio,
    Video,
    Document,
}

/// A validated media type (`type/subtype`), never a free-form string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MimeType(String);

impl MimeType {
    /// Validates a `type/subtype` media type.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the value is not a valid MIME type.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let valid = value.len() <= 255
            && value.split_once('/').is_some_and(|(kind, subtype)| {
                !kind.is_empty()
                    && !subtype.is_empty()
                    && kind
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
                    && subtype
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+' | '_'))
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(Error::InvalidInput(format!("mime type: {value}")))
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for MimeType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for MimeType {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        Self::new(value)
    }
}

impl Serialize for MimeType {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for MimeType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A vendor-hosted file reference. Expired references are re-uploaded from the
/// blob store by the engine when the blob is still available (FR-PART-4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFileRef {
    pub provider: ProviderId,
    pub file_id: String,
    pub expires_at: Option<DateTime<Utc>>,
    /// Durable fallback for the file's bytes, so an expired reference can be
    /// revived without the vendor (FR-PART-4). Absent means the bytes are gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobId>,
}

impl ProviderFileRef {
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .is_some_and(|expires_at| expires_at <= Utc::now())
    }
}

/// Where a media part's bytes live.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "source")]
#[non_exhaustive]
pub enum MediaSource {
    Blob(BlobId),
    Url(String),
    ProviderFile(ProviderFileRef),
    #[serde(with = "codec::bytes_base64")]
    Inline(Vec<u8>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MediaMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub extension: std::collections::BTreeMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPart {
    pub kind: MediaKind,
    pub source: MediaSource,
    pub mime: MimeType,
    #[serde(default)]
    pub metadata: MediaMetadata,
}

impl MediaPart {
    #[must_use]
    pub fn new(kind: MediaKind, source: MediaSource, mime: MimeType) -> Self {
        Self {
            kind,
            source,
            mime,
            metadata: MediaMetadata::default(),
        }
    }

    /// Inline payload size in bytes, when the bytes travel with the part.
    #[must_use]
    pub fn inline_len(&self) -> usize {
        match &self.source {
            MediaSource::Inline(bytes) => bytes.len(),
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_types_are_validated() {
        assert!(MimeType::new("image/png").is_ok());
        assert!(MimeType::new("audio/wav").is_ok());
        assert!(MimeType::new("image").is_err());
        assert!(MimeType::new("image/").is_err());
        assert!(MimeType::new("/png").is_err());
        assert!(MimeType::new("image/png; charset=utf-8").is_err());
    }
}
