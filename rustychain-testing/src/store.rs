use std::collections::HashMap;

use rustychain_core::{
    ApprovalId, ApprovalRecord, Commit, Error, Node, NodeId, Result, Session, SessionId, Store,
};
use tokio::sync::RwLock;

#[derive(Default)]
struct Inner {
    sessions: HashMap<SessionId, Session>,
    nodes: HashMap<NodeId, Node>,
    children: HashMap<NodeId, Vec<NodeId>>,
    approvals: HashMap<ApprovalId, ApprovalRecord>,
}

/// A complete in-memory [`Store`].
///
/// It enforces the same contract as a durable backend: one commit is atomic,
/// `expected_version` gates writes and applies with an exclusive lock.
#[derive(Default)]
pub struct InMemoryStore {
    inner: RwLock<Inner>,
}

impl InMemoryStore {
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
        let mut inner = self.inner.write().await;

        let mut session = match commit.expected_version {
            None => {
                if let Some(existing) = inner.sessions.get(&session_id) {
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
                let existing = inner
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
            if inner.nodes.contains_key(&node.id) {
                return Err(Error::InvalidInput(format!(
                    "node {} already exists",
                    node.id
                )));
            }
        }
        for node in &commit.nodes {
            inner.children.entry(node.id).or_default();
            if let Some(parent) = node.parent {
                inner.children.entry(parent).or_default().push(node.id);
            }
        }
        for node in &commit.nodes {
            inner.nodes.insert(node.id, node.clone());
        }

        commit.apply(&mut session);
        inner.sessions.insert(session.id, session.clone());
        Ok(session)
    }

    async fn load(&self, session: SessionId) -> Result<Session> {
        self.inner
            .read()
            .await
            .sessions
            .get(&session)
            .cloned()
            .ok_or(Error::SessionNotFound(session))
    }

    async fn node(&self, id: NodeId) -> Result<Node> {
        self.inner
            .read()
            .await
            .nodes
            .get(&id)
            .cloned()
            .ok_or(Error::NodeNotFound(id))
    }

    async fn ancestors(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let inner = self.inner.read().await;
        if !inner.nodes.contains_key(&id) {
            return Err(Error::NodeNotFound(id));
        }
        let mut chain = Vec::new();
        let mut cursor = Some(id);
        while let Some(current) = cursor {
            chain.push(current);
            cursor = inner.nodes.get(&current).and_then(|node| node.parent);
        }
        chain.reverse();
        Ok(chain)
    }

    async fn children(&self, id: NodeId) -> Result<Vec<NodeId>> {
        Ok(self
            .inner
            .read()
            .await
            .children
            .get(&id)
            .cloned()
            .unwrap_or_default())
    }

    async fn save_approval(&self, record: ApprovalRecord) -> Result<()> {
        self.inner.write().await.approvals.insert(record.id, record);
        Ok(())
    }

    async fn approval(&self, id: ApprovalId) -> Result<Option<ApprovalRecord>> {
        Ok(self.inner.read().await.approvals.get(&id).cloned())
    }

    async fn pending_approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>> {
        let mut records: Vec<ApprovalRecord> = self
            .inner
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
    use std::collections::VecDeque;

    use super::*;

    #[tokio::test]
    async fn conflict_on_stale_version() {
        let store = InMemoryStore::new();
        let session_id = SessionId::new();
        let commit = Commit {
            session_id,
            expected_version: None,
            nodes: Vec::new(),
            head: None,
            phase: rustychain_core::Phase::Idle,
            turn: 0,
            queue: VecDeque::default(),
            grants: Vec::new(),
            metadata: None,
        };
        let created = store.commit(commit.clone()).await.expect("create");
        assert_eq!(created.version, 1);
        assert!(matches!(
            store.commit(commit).await,
            Err(Error::StoreConflict { .. })
        ));
    }
}
