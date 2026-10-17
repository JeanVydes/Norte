use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};
use crate::id::{ApprovalId, CallId, Digest, GrantId, SessionId, ToolName};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalStatus {
    Pending,
    Granted,
    Denied,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "scope")]
#[non_exhaustive]
pub enum GrantScope {
    Call(CallId),
    Tool(ToolName),
    Session(SessionId),
    Global,
}

/// A durable authorization. Revocation is recorded, never erased.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub scope: GrantScope,
    pub issued_at: DateTime<Utc>,
    pub issued_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Grant {
    pub fn new(scope: GrantScope, issued_by: impl Into<String>) -> Self {
        Self {
            id: GrantId::new(),
            scope,
            issued_at: Utc::now(),
            issued_by: issued_by.into(),
            revoked_at: None,
        }
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditAction {
    Requested,
    Granted,
    Denied,
    Revoked,
    Edited,
    Expired,
    Replayed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub action: AuditAction,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A recorded argument edit. The digest of the edited arguments is stored so a
/// grant issued for the original call cannot be reused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalEdit {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub arguments: Value,
    pub digest: Digest,
}

/// First-class approval record, bound to the digest of the exact arguments the
/// host is authorizing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    pub id: ApprovalId,
    pub session: SessionId,
    pub call_id: CallId,
    pub tool: ToolName,
    pub argument_digest: Digest,
    pub requested_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub status: ApprovalStatus,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<ApprovalEdit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit: Vec<AuditEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ApprovalRecord {
    #[must_use]
    pub fn new(
        session: SessionId,
        call_id: CallId,
        tool: ToolName,
        arguments: &Value,
        lifetime: Duration,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: ApprovalId::new(),
            session,
            call_id,
            tool,
            argument_digest: Digest::of_json(arguments),
            requested_at: now,
            expires_at: now + lifetime,
            status: ApprovalStatus::Pending,
            edits: Vec::new(),
            grants: Vec::new(),
            audit: vec![AuditEntry {
                at: now,
                actor: "engine".to_owned(),
                action: AuditAction::Requested,
                detail: None,
            }],
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at
    }

    /// Rejects a record whose arguments no longer match the digest the approval
    /// was issued for.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalDenied`] when the digest differs.
    pub fn verify_arguments(&self, arguments: &Value) -> Result<()> {
        if Digest::of_json(arguments) == self.argument_digest {
            Ok(())
        } else {
            Err(Error::ApprovalDenied(
                "arguments do not match the approved digest".to_owned(),
            ))
        }
    }

    pub fn grant(&mut self, scope: GrantScope, actor: impl Into<String>) {
        let actor = actor.into();
        self.grants.push(Grant::new(scope, actor.clone()));
        self.status = ApprovalStatus::Granted;
        self.audit.push(AuditEntry {
            at: Utc::now(),
            actor,
            action: AuditAction::Granted,
            detail: None,
        });
    }

    pub fn deny(&mut self, actor: impl Into<String>, reason: impl Into<String>) {
        let actor = actor.into();
        self.status = ApprovalStatus::Denied;
        self.audit.push(AuditEntry {
            at: Utc::now(),
            actor,
            action: AuditAction::Denied,
            detail: Some(reason.into()),
        });
    }

    pub fn edit(&mut self, actor: impl Into<String>, arguments: Value) {
        let actor = actor.into();
        let digest = Digest::of_json(&arguments);
        self.argument_digest = digest;
        self.edits.push(ApprovalEdit {
            at: Utc::now(),
            actor: actor.clone(),
            arguments,
            digest,
        });
        self.audit.push(AuditEntry {
            at: Utc::now(),
            actor,
            action: AuditAction::Edited,
            detail: None,
        });
    }

    pub fn revoke(&mut self, grant: GrantId, actor: impl Into<String>) {
        let actor = actor.into();
        let now = Utc::now();
        if let Some(grant) = self
            .grants
            .iter_mut()
            .find(|candidate| candidate.id == grant)
        {
            grant.revoked_at = Some(now);
        }
        self.audit.push(AuditEntry {
            at: now,
            actor,
            action: AuditAction::Revoked,
            detail: Some(grant.to_string()),
        });
    }

    /// Whether the record currently authorizes these exact arguments.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalDenied`] when the record was denied, has no
    /// active matching grant, or the arguments differ from the approved digest,
    /// and [`Error::ApprovalExpired`] once the record has expired.
    pub fn authorize(&self, now: DateTime<Utc>, arguments: &Value) -> Result<()> {
        if self.status == ApprovalStatus::Denied {
            return Err(Error::ApprovalDenied("approval was denied".to_owned()));
        }
        if self.is_expired_at(now) {
            return Err(Error::ApprovalExpired(self.id));
        }
        self.verify_arguments(arguments)?;
        let active = self.grants.iter().any(|grant| {
            grant.is_active() && grant.applies_to(self.session, self.call_id, &self.tool)
        });
        if active {
            Ok(())
        } else {
            Err(Error::ApprovalDenied(
                "no active grant matches this call".to_owned(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> (ApprovalRecord, Value) {
        let arguments = serde_json::json!({ "path": "/tmp/a" });
        let record = ApprovalRecord::new(
            SessionId::new(),
            CallId::new(),
            ToolName::new("read_file").expect("tool"),
            &arguments,
            Duration::minutes(10),
        );
        (record, arguments)
    }

    #[test]
    fn grant_authorizes_the_same_arguments() {
        let (mut record, arguments) = record();
        record.grant(GrantScope::Call(record.call_id), "host");
        assert!(record.authorize(Utc::now(), &arguments).is_ok());
    }

    #[test]
    fn tampered_arguments_invalidate_the_grant() {
        let (mut record, _) = record();
        record.grant(GrantScope::Call(record.call_id), "host");
        let tampered = serde_json::json!({ "path": "/etc/shadow" });
        assert!(record.authorize(Utc::now(), &tampered).is_err());
    }

    #[test]
    fn expired_approvals_are_rejected() {
        let (record, arguments) = record();
        let later = record.expires_at + Duration::seconds(1);
        assert!(matches!(
            record.authorize(later, &arguments),
            Err(Error::ApprovalExpired(_))
        ));
    }

    #[test]
    fn revoked_grants_stop_authorizing() {
        let (mut record, arguments) = record();
        record.grant(GrantScope::Call(record.call_id), "host");
        let grant = record.grants[0].id;
        record.revoke(grant, "host");
        assert!(record.authorize(Utc::now(), &arguments).is_err());
    }
}
