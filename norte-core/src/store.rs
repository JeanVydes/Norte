use crate::approval::ApprovalRecord;
use crate::domain::commit::Commit;
use crate::domain::message::Message;
use crate::domain::node::Node;
use crate::domain::session::Session;
use crate::error::Result;
use crate::id::{ApprovalId, BlobId, NodeId, SessionId};

/// Persistence port for sessions, the conversation graph and approvals.
///
/// Implementations must make one `commit` atomic and enforce optimistic
/// concurrency through `expected_version`.
#[async_trait::async_trait]
pub trait Store: Send + Sync {
    /// Applies one commit. Returns the new session state on success and
    /// [`crate::Error::StoreConflict`] when the version does not match.
    async fn commit(&self, commit: Commit) -> Result<Session>;

    async fn load(&self, session: SessionId) -> Result<Session>;

    async fn node(&self, id: NodeId) -> Result<Node>;

    /// Ancestor chain from the root down to `id`, inclusive.
    async fn ancestors(&self, id: NodeId) -> Result<Vec<NodeId>>;

    async fn children(&self, id: NodeId) -> Result<Vec<NodeId>>;

    async fn save_approval(&self, record: ApprovalRecord) -> Result<()>;

    async fn approval(&self, id: ApprovalId) -> Result<Option<ApprovalRecord>>;

    /// Every approval record recorded for the session, decided or not, oldest
    /// first. The engine reads it to resolve a call; the host-facing view of
    /// what still needs a decision is [`crate::Engine::pending_approvals`].
    async fn approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>>;

    /// The conversation the model sees: every ancestor message, root first.
    async fn history(&self, head: Option<NodeId>) -> Result<Vec<Message>> {
        let Some(head) = head else {
            return Ok(Vec::new());
        };
        let mut messages = Vec::new();
        for id in self.ancestors(head).await? {
            messages.push(self.node(id).await?.message);
        }
        Ok(messages)
    }
}

/// Content-addressed blob storage for media that must not live in graph nodes.
#[async_trait::async_trait]
pub trait BlobStore: Send + Sync {
    /// Idempotent: the same bytes always produce the same id.
    async fn put(&self, bytes: &[u8]) -> Result<BlobId>;

    async fn get(&self, id: BlobId) -> Result<Vec<u8>>;

    async fn contains(&self, id: BlobId) -> Result<bool>;
}
