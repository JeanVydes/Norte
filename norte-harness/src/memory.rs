//! Ready-to-use in-memory backends for the [`Store`] and [`BlobStore`] ports.
//!
//! They implement the full contract — atomic commits with optimistic
//! concurrency, the ancestor graph, the approval ledger, content-addressed
//! bytes — so an application can run without any external service. The
//! trade-off is the obvious one: state lives in the process and leaves with it.
//!
//! Reach for them in tests, CLIs, desktop applications, prototypes and
//! single-process workers. When a session must survive a restart, implement
//! [`Store`] and [`BlobStore`] over your own database and object storage; the
//! engine only ever sees those two contracts.

use std::collections::HashMap;

use tokio::sync::RwLock;

use crate::approval::ApprovalRecord;
use crate::store::Store;
use norte_core::Commit;
use norte_core::{ApprovalId, Error, Node, NodeId, Result, Session, SessionId};

#[derive(Default)]
struct Tables {
    sessions: HashMap<SessionId, Session>,
    nodes: HashMap<NodeId, Node>,
    children: HashMap<NodeId, Vec<NodeId>>,
    approvals: HashMap<ApprovalId, ApprovalRecord>,
}

/// A complete in-memory [`Store`].
///
/// It enforces the same contract a durable backend must: one commit is atomic,
/// `expected_version` gates every write, and the whole thing is guarded by an
/// exclusive lock.
#[derive(Default)]
pub struct InMemoryStore {
    tables: RwLock<Tables>,
}

impl InMemoryStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl Store for InMemoryStore {
    async fn commit(&self, commit: Commit) -> Result<Session> {
        commit.validate()?;
        let session_id = commit.session_id;
        let mut tables = self.tables.write().await;

        let mut session = match commit.expected_version {
            None => {
                if let Some(existing) = tables.sessions.get(&session_id) {
                    return Err(Error::StoreConflict {
                        session: session_id,
                        expected: None,
                        actual: existing.version,
                    });
                }
                let mut session = Session::new();
                session.id = session_id;
                session
            }
            Some(expected) => {
                let existing = tables
                    .sessions
                    .get(&session_id)
                    .cloned()
                    .ok_or(Error::SessionNotFound(session_id))?;
                if existing.version != expected {
                    return Err(Error::StoreConflict {
                        session: session_id,
                        expected: Some(expected),
                        actual: existing.version,
                    });
                }
                existing
            }
        };

        for node in &commit.nodes {
            if tables.nodes.contains_key(&node.id) {
                return Err(Error::InvalidInput(format!(
                    "node {} already exists",
                    node.id
                )));
            }
        }
        for node in &commit.nodes {
            tables.children.entry(node.id).or_default();
            if let Some(parent) = node.parent {
                tables.children.entry(parent).or_default().push(node.id);
            }
        }
        for node in &commit.nodes {
            tables.nodes.insert(node.id, node.clone());
        }

        commit.apply(&mut session);
        tables.sessions.insert(session.id, session.clone());
        Ok(session)
    }

    async fn load(&self, session: SessionId) -> Result<Session> {
        self.tables
            .read()
            .await
            .sessions
            .get(&session)
            .cloned()
            .ok_or(Error::SessionNotFound(session))
    }

    async fn node(&self, id: NodeId) -> Result<Node> {
        self.tables
            .read()
            .await
            .nodes
            .get(&id)
            .cloned()
            .ok_or(Error::NodeNotFound(id))
    }

    async fn ancestors(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let tables = self.tables.read().await;
        if !tables.nodes.contains_key(&id) {
            return Err(Error::NodeNotFound(id));
        }
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            chain.push(current);
            cursor = tables.nodes.get(&current).and_then(|node| node.parent);
        }
        chain.reverse();
        Ok(chain)
    }

    async fn children(&self, id: NodeId) -> Result<Vec<NodeId>> {
        Ok(self
            .tables
            .read()
            .await
            .children
            .get(&id)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_approval(&self, record: ApprovalRecord) -> Result<()> {
        self.tables
            .write()
            .await
            .approvals
            .insert(record.id, record);
        Ok(())
    }

    async fn approval(&self, id: ApprovalId) -> Result<Option<ApprovalRecord>> {
        Ok(self.tables.read().await.approvals.get(&id).cloned())
    }

    async fn approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>> {
        let mut records: Vec<ApprovalRecord> = self
            .tables
            .read()
            .await
            .approvals
            .values()
            .filter(|record| record.session == session)
            .cloned()
            .collect();
        records.sort_by_key(|record| record.requested_at);
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_core::Phase;

    fn creation(session_id: SessionId) -> Commit {
        Commit {
            session_id,
            expected_version: None,
            nodes: Vec::new(),
            head: None,
            phase: Phase::Idle,
            turn: 0,
            grants: Vec::new(),
            metadata: None,
        }
    }

    #[tokio::test]
    async fn a_stale_version_is_rejected() {
        let store = InMemoryStore::new();
        let session_id = SessionId::new();
        let created = store
            .commit(creation(session_id))
            .await
            .expect("create session");
        assert_eq!(created.version, 1);
        assert!(matches!(
            store.commit(creation(session_id)).await,
            Err(Error::StoreConflict { .. })
        ));
    }
}
