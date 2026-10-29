use std::fmt::{Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::error::{Error, Result};

macro_rules! entity_id {
    ($name:ident) => {
        /// An opaque, randomly generated identifier.
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// A fresh, random identifier.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// The identifier as a UUID.
            #[must_use]
            pub fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                Display::fmt(&self.0, f)
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self> {
                Uuid::parse_str(value)
                    .map(Self)
                    .map_err(|error| Error::InvalidId(format!("{}: {error}", stringify!($name))))
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }

        impl AsRef<Uuid> for $name {
            fn as_ref(&self) -> &Uuid {
                &self.0
            }
        }
    };
}

entity_id!(NodeId);
entity_id!(SessionId);
entity_id!(CallId);
entity_id!(GenerationId);

const HEX: &[u8; 16] = b"0123456789abcdef";

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(HEX[usize::from(byte >> 4)]));
        out.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    out
}

fn decode_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = char::from(chunk[0]).to_digit(16)?;
        let low = char::from(chunk[1]).to_digit(16)?;
        out[index] = u8::try_from(high * 16 + low).ok()?;
    }
    Some(out)
}

macro_rules! content_hash {
    ($name:ident) => {
        /// A content hash: 32 bytes of SHA-256, lowercase hex on the wire.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name([u8; 32]);

        impl $name {
            /// The hash of `bytes`.
            #[must_use]
            pub fn of(bytes: &[u8]) -> Self {
                Self(Sha256::digest(bytes).into())
            }

            /// The 32 raw bytes.
            #[must_use]
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// The hash in lowercase hex.
            #[must_use]
            pub fn to_hex(self) -> String {
                encode_hex(&self.0)
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&encode_hex(&self.0))
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self> {
                decode_hex(value).map(Self).ok_or_else(|| {
                    Error::InvalidId(format!("{}: not a 32-byte hex hash", stringify!($name)))
                })
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&encode_hex(&self.0))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                decode_hex(&value).map(Self).ok_or_else(|| {
                    serde::de::Error::custom(format!(
                        "{}: expected a 32-byte hex hash",
                        stringify!($name)
                    ))
                })
            }
        }
    };
}

content_hash!(BlobId);

impl schemars::JsonSchema for BlobId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("BlobId")
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed(concat!(module_path!(), "::BlobId"))
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "description": "A 32-byte SHA-256 content hash in lowercase hex.",
        })
    }
}
content_hash!(Digest);

impl Digest {
    /// The hash of `value`'s canonical JSON text.
    #[must_use]
    pub fn of_json(value: &serde_json::Value) -> Self {
        Self::of(value.to_string().as_bytes())
    }
}

macro_rules! name_id {
    ($name:ident, $label:literal, $check:expr) => {
        /// A validated name identifier, stored as written.
        #[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            /// Validates and stores the identifier.
            ///
            /// # Errors
            ///
            /// Returns [`crate::Error::InvalidId`] when the value is empty, too
            /// long, or contains characters outside the allowed set.
            pub fn new(value: impl Into<String>) -> Result<Self> {
                let value = value.into();
                let valid = !value.is_empty() && value.len() <= 128 && $check(&value);
                if valid {
                    Ok(Self(value))
                } else {
                    Err(Error::InvalidId(format!("{}: {value}", $label)))
                }
            }

            /// The identifier as a string.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Display for $name {
            fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = Error;

            fn from_str(value: &str) -> Result<Self> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = Error;

            fn try_from(value: &str) -> Result<Self> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = Error;

            fn try_from(value: String) -> Result<Self> {
                Self::new(value)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::new(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

name_id!(ProviderId, "provider id", |value: &str| value.chars().all(
    |c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
));

name_id!(ModelId, "model id", |value: &str| value.chars().all(|c| {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@')
}));

name_id!(ToolName, "tool name", |value: &str| value
    .chars()
    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));

name_id!(WireFamily, "wire family", |value: &str| value.chars().all(
    |c| { c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') }
));

/// A provider and model pair. The only way to name a model across the API.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelRef {
    /// The provider that owns the model.
    pub provider: ProviderId,
    /// The model's id within the provider.
    pub model: ModelId,
}

impl ModelRef {
    /// A reference to `model` on `provider`.
    #[must_use]
    pub fn new(provider: ProviderId, model: ModelId) -> Self {
        Self { provider, model }
    }
}

impl Display for ModelRef {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

impl FromStr for ModelRef {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self> {
        let (provider, model) = value
            .split_once('/')
            .ok_or_else(|| Error::InvalidId(format!("model ref: {value}")))?;
        Ok(Self::new(ProviderId::new(provider)?, ModelId::new(model)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_ids_round_trip() {
        let id = NodeId::new();
        assert_eq!(NodeId::from_str(&id.to_string()).ok(), Some(id));
    }

    #[test]
    fn model_ref_round_trips() {
        let value: ModelRef = "anthropic/claude-sonnet-4-5".parse().expect("valid ref");
        assert_eq!(value.provider.as_str(), "anthropic");
        assert_eq!(value.model.as_str(), "claude-sonnet-4-5");
        assert_eq!(value.to_string(), "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn model_ref_rejects_missing_provider() {
        assert!(ModelRef::from_str("claude").is_err());
    }

    #[test]
    fn blob_id_is_stable() {
        assert_eq!(BlobId::of(b"abc"), BlobId::of(b"abc"));
        assert_ne!(BlobId::of(b"abc"), BlobId::of(b"abd"));
        let id = BlobId::of(b"abc");
        assert_eq!(BlobId::from_str(&id.to_string()).ok(), Some(id));
    }

    #[test]
    fn invalid_tool_names_are_rejected() {
        assert!(ToolName::new("read_file").is_ok());
        assert!(ToolName::new("read file").is_err());
        assert!(ToolName::new("").is_err());
    }
}
