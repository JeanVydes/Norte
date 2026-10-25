//! Durable authorization: how far a permission reaches, and to what.
//!
//! A grant is part of the *data*: [`Session`](crate::domain::session::Session)
//! and [`Commit`](crate::domain::commit::Commit) both carry the grants in force,
//! so the type lives here with the rest of what a provider may serialise. The
//! record of a *decision* — what was parked, who decided, what the audit trail
//! says — belongs to the runtime that parks and resumes, and lives in
//! `norte-harness` instead.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::id::{CallId, GrantId, SessionId, ToolName};

/// How wide a grant reaches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
#[non_exhaustive]
pub enum GrantScope {
    /// The one call.
    Call(CallId),
    /// Any call to that tool.
    Tool(ToolName),
    /// Any tool in that session.
    Session(SessionId),
    /// Anything the engine asks this record about.
    Global,
}

/// A durable authorization. Revocation is recorded, never erased.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The grant's id.
    pub id: GrantId,
    /// How far the grant reaches.
    pub scope: GrantScope,
    /// When it was issued.
    pub issued_at: DateTime<Utc>,
    /// Who issued it.
    pub issued_by: String,
    /// When it was revoked, if it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Grant {
    /// A grant for `scope`, issued by `issued_by`.
    #[must_use]
    pub fn new(scope: GrantScope, issued_by: impl Into<String>) -> Self {
        Self {
            id: GrantId::new(),
            scope,
            issued_at: Utc::now(),
            issued_by: issued_by.into(),
            revoked_at: None,
        }
    }

    /// Whether the grant has not been revoked.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

    /// Whether the grant authorizes this call.
    #[must_use]
    pub fn applies_to(&self, session: SessionId, call: CallId, tool: &ToolName) -> bool {
        if !self.is_active() {
            return false;
        }
        match &self.scope {
            GrantScope::Call(id) => *id == call,
            GrantScope::Tool(name) => name == tool,
            GrantScope::Session(id) => *id == session,
            GrantScope::Global => true,
        }
    }
}
