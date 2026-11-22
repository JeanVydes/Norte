use crate::error::Result;

use super::user_message_draft::UserMessageDraft;

/// Something a tool asks the host to do beyond answering its own call.
///
/// Effects are host instructions, never provider content. They are skipped when
/// a tool result is serialized, and the host decides whether a policy allows
/// each one before it acts.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum ToolEffect {
    /// Add a user-role message to the conversation.
    UserMessage(UserMessageDraft),
}

impl ToolEffect {
    /// Builds a user-message effect from validated parts.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::InvalidInput`] when the parts are not a valid
    /// user message for a tool to request.
    pub fn user_message(parts: Vec<crate::domain::part::Part>) -> Result<Self> {
        Ok(Self::UserMessage(UserMessageDraft::new(parts)?))
    }
}
