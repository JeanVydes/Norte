use std::str::FromStr;

use rustychain_core::{Message, ModelRef};

/// Parses a `provider/model` reference, panicking only on a caller bug.
///
/// # Panics
///
/// Panics when the reference is not a valid `provider/model` pair.
#[must_use]
pub fn model(reference: &str) -> ModelRef {
    match ModelRef::from_str(reference) {
        Ok(reference) => reference,
        Err(error) => panic!("invalid model reference {reference:?}: {error}"),
    }
}

#[must_use]
pub fn user(text: impl Into<String>) -> Message {
    Message::user(text)
}

#[must_use]
pub fn system(text: impl Into<String>) -> Message {
    Message::system(text)
}
