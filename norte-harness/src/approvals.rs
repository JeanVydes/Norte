//! The approval surface: what the host decides about a call that a policy
//! parked, and the grants it holds afterwards.

use chrono::Utc;

use crate::approval::{ApprovalRecord, ApprovalStatus};
use crate::domain::commit::Commit;
use crate::domain::session::Phase;
use crate::event::EngineEvent;
use crate::grant::GrantScope;
use norte_core::error::{Error, Result};
use norte_core::id::{ApprovalId, GrantId, SessionId};

use super::Harness;

impl Harness {
    /// The approvals the host still has to decide for the session: records that
    /// are pending and have not expired, oldest first. Decided and expired
    /// records stay in the store for audit rather than showing up here.
    ///
    /// # Errors
    ///
    /// Propagates the store error.
    pub async fn pending_approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>> {
        let now = Utc::now();
        Ok(self
            .store
            .approvals(session)
            .await?
            .into_iter()
            .filter(|record| record.status == ApprovalStatus::Pending && !record.is_expired_at(now))
            .collect())
    }

    /// Grants an approval and records the grant.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalNotFound`] when no such record exists, or the
    /// store error from saving the grant and the follow-up commit.
    pub async fn approve(
        &self,
        approval: ApprovalId,
        scope: GrantScope,
        actor: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let mut record = self
            .store
            .approval(approval)
            .await?
            .ok_or(Error::ApprovalNotFound(approval))?;
        record.grant(scope, actor);
        self.store.save_approval(record.clone()).await?;

        let session = self.store.load(record.session).await?;
        let mut grants = session.grants.clone();
        grants.extend(record.grants.iter().cloned());
        let commit = Commit {
            session_id: session.id,
            expected_version: Some(session.version),
            nodes: Vec::new(),
            head: None,
            phase: Phase::AwaitingTool,
            turn: session.turn,
            grants,
            metadata: None,
        };
        self.store.commit(commit).await?;
        self.observers.emit(&EngineEvent::ApprovalGranted {
            session: record.session,
            approval: record.id,
        });
        Ok(record)
    }

    /// Records a denial and audits it.
    ///
    /// # Errors
    ///
    /// Returns [`Error::ApprovalNotFound`] when no such record exists, or the
    /// store error from persisting the denial.
    pub async fn deny(
        &self,
        approval: ApprovalId,
        actor: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let mut record = self
            .store
            .approval(approval)
            .await?
            .ok_or(Error::ApprovalNotFound(approval))?;
        record.deny(actor, reason);
        self.store.save_approval(record.clone()).await?;
        Ok(record)
    }

    /// Revokes a grant and audits it.
    ///
    /// The grant is dropped from the session's standing grants in the same call,
    /// so a tool-, session- or global-scoped grant stops authorizing calls the
    /// moment this returns (FR-APPR-3).
    ///
    /// # Errors
    ///
    /// Returns [`Error::GrantNotFound`] when no record of the session holds that
    /// grant, and propagates store failures.
    pub async fn revoke(
        &self,
        session: SessionId,
        grant: GrantId,
        actor: impl Into<String>,
    ) -> Result<ApprovalRecord> {
        let records = self.store.approvals(session).await?;
        let mut record = records
            .into_iter()
            .find(|record| record.grants.iter().any(|held| held.id == grant))
            .ok_or(Error::GrantNotFound(grant))?;
        record.revoke(grant, actor);
        self.store.save_approval(record.clone()).await?;

        let revoked_at = record
            .grants
            .iter()
            .find(|held| held.id == grant)
            .and_then(|held| held.revoked_at);
        let current = self.store.load(session).await?;
        let mut grants = current.grants.clone();
        for held in &mut grants {
            if held.id == grant {
                held.revoked_at = revoked_at;
            }
        }
        self.store
            .commit(Commit {
                session_id: session,
                expected_version: Some(current.version),
                nodes: Vec::new(),
                head: None,
                phase: current.phase,
                turn: current.turn,
                grants,
                metadata: None,
            })
            .await?;
        self.observers.emit(&EngineEvent::ApprovalRevoked {
            session,
            approval: record.id,
            grant,
        });
        Ok(record)
    }
}
