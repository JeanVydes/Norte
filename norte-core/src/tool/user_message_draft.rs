use crate::domain::part::Part;
use crate::error::{Error, Result};

/// A user-role message a tool asks the host to add to the conversation.
///
/// The draft is validated on construction so the host never has to ask whether
/// a plugin tried to smuggle a tool call or an instruction-bearing system role
/// into a message that will be sent as the human's turn. The host still owns
/// provenance, limits and the decision to commit it.
#[derive(Clone, Debug, PartialEq)]
pub struct UserMessageDraft {
    parts: Vec<Part>,
}

impl UserMessageDraft {
    /// Builds a draft from message parts.
    ///
    /// Only text and media are allowed. An empty draft, empty text and any
    /// other part kind are refused with [`Error::InvalidInput`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the parts do not describe a message
    /// a tool may ask the host to inject.
    pub fn new(parts: Vec<Part>) -> Result<Self> {
        if parts.is_empty() {
            return Err(Error::InvalidInput(
                "a user message effect needs at least one part".to_owned(),
            ));
        }
        for part in &parts {
            match part {
                Part::Media(_) => {}
                other => {
                    let text = other.as_text().unwrap_or_default();
                    if !matches!(other, Part::Text(_)) || text.trim().is_empty() {
                        return Err(Error::InvalidInput(
                            "a user message effect carries only non-empty text or media".to_owned(),
                        ));
                    }
                }
            }
        }
        Ok(Self { parts })
    }

    /// Builds a draft carrying one text part.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidInput`] when the text is empty.
    pub fn text(text: impl Into<String>) -> Result<Self> {
        Self::new(vec![Part::text(text)])
    }

    /// The validated parts.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Takes the validated parts for the host to adapt and commit.
    #[must_use]
    pub fn into_parts(self) -> Vec<Part> {
        self.parts
    }

    /// The total size of inline media bytes in the draft.
    #[must_use]
    pub fn inline_len(&self) -> usize {
        self.parts
            .iter()
            .filter_map(|part| match part {
                Part::Media(media) => Some(media.inline_len()),
                _ => None,
            })
            .sum()
    }
}
