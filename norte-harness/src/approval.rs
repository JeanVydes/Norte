//! The record of an approval decision: what was parked, who decided, how.
//!
//! One consumer, the runtime: the store it persists through is the only thing
//! that reads or writes these. The grant itself — how far a permission reaches —
//! is data the wire carries, and lives in [`norte_core::approval`] with the rest.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use norte_core::approval::{Grant, GrantScope};
use norte_core::error::{Error, Result};
use norte_core::id::{ApprovalId, CallId, Digest, GrantId, SessionId, ToolName};

/// Where an approval record stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ApprovalStatus {
    /// Waiting for a decision
    Pending,
    /// A grant was issued.
    Granted,
    /// A human denied the call.
    Denied,
    /// The record's time to live elapsed before a decision.
    Expired,
}

/// What happened to an approval record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditAction {
    /// The engine parked the call.
    Requested,
    /// A grant was issued.
    Granted,
    /// A human denied the call.
    Denied,
    /// A grant was revoked.
    Revoked,
    /// The arguments were edited.
    Edited,
    /// The record expired.
    Expired,
    /// A stored decision was replayed.
    Replayed,
}

/// One entry of a record's audit trail.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// When it happened.
    pub at: DateTime<Utc>,
    /// Who did it.
    pub actor: String,
    /// What happened.
    pub action: AuditAction,
    /// Anything else worth keeping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A recorded argument edit. The digest of the edited arguments is stored so a
/// grant issued for the original call cannot be reused.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalEdit {
    /// When the edit was made.
    pub at: DateTime<Utc>,
    /// Who made it.
    pub actor: String,
    /// The arguments after the edit.
    pub arguments: Value,
    /// The digest of the edited arguments.
    pub digest: Digest,
}

/// First-class approval record, bound to the digest of the exact arguments the
/// host is authorizing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRecord {
    /// The record's id.
    pub id: ApprovalId,
    /// The session the call belongs to.
    pub session: SessionId,
    /// The call awaiting a decision.
    pub call_id: CallId,
    /// The tool that wants to run.
    pub tool: ToolName,
    /// The digest of the exact arguments being authorized.
    pub argument_digest: Digest,
    /// The exact arguments being authorized, after any policy transform.
    ///
    /// This is what the host should show and what the engine runs on a grant.
    /// It is `None` only for records written before the field existed; a host
    /// should then fall back to the call in history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Value>,
    /// When the decision was requested.
    pub requested_at: DateTime<Utc>,
    /// When the request stops being decidable.
    pub expires_at: DateTime<Utc>,
    /// Where the record stands.
    pub status: ApprovalStatus,
    /// The edits made to the arguments, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<ApprovalEdit>,
    /// The grants issued from this record.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
    /// The audit trail, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audit: Vec<AuditEntry>,
    /// Free-form data the host attached to the record.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
}

impl ApprovalRecord {
    /// A pending record for a call, alive for `lifetime`.
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
            arguments: Some(arguments.clone()),
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

    /// Whether the record is expired at `now`.
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

    /// Issues a grant for `scope` and records it.
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

    /// Denies the call with a reason.
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

    /// Replaces the approved arguments, recording who did it.
    pub fn edit(&mut self, actor: impl Into<String>, arguments: Value) {
        let actor = actor.into();
        let digest = Digest::of_json(&arguments);
        self.argument_digest = digest;
        self.arguments = Some(arguments.clone());
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

    /// Revokes the grant with that id, recording who did it.
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
    fn a_record_keeps_the_arguments_it_approves() {
        let (record, arguments) = record();
        assert_eq!(record.arguments, Some(arguments));
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
