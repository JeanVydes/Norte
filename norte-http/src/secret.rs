use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A value that must never leak into logs or `Debug` output.
///
/// The inner value is only reachable through [`Secret::expose`] or
/// [`Secret::into_inner`], so an accidental `{:?}` prints `Secret(<redacted>)`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value`.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwraps the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl<T: Serialize> Serialize for Secret<T> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Secret<T> {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        T::deserialize(deserializer).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_but_expose_returns_the_value() {
        let secret = Secret::new("sk-live-123");
        assert_eq!(format!("{secret:?}"), "Secret(<redacted>)");
        assert_eq!(*secret.expose(), "sk-live-123");
        assert_eq!(secret.into_inner(), "sk-live-123");
    }

    #[test]
    fn serializes_transparently() {
        let secret = Secret::new("token");
        assert_eq!(
            serde_json::to_string(&secret).expect("serialize"),
            "\"token\""
        );
        let back: Secret<String> = serde_json::from_str("\"token\"").expect("deserialize");
        assert_eq!(*back.expose(), "token");
    }
}
