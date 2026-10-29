# Persist a session

The in-memory pair is a real implementation, not a stub. Reach for your own
`Store` when a session has to outlive the process.

## What you have to implement

```rust,ignore
#[async_trait::async_trait]
impl Store for MyStore {
    async fn commit(&self, commit: Commit) -> Result<Session> { /* one transaction */ }
    async fn load(&self, session: SessionId) -> Result<Session> { /* .. */ }
    async fn node(&self, node: NodeId) -> Result<Node> { /* .. */ }
    async fn ancestors(&self, node: NodeId) -> Result<Vec<NodeId>> { /* .. */ }
    async fn children(&self, node: NodeId) -> Result<Vec<NodeId>> { /* .. */ }
}
```

`history()` has a default implementation built from `ancestors` and `node`; keep
it unless you have a better index.

## The contract that matters

**`commit` is compare-and-swap.** The `Commit` carries the `expected_version` it
was built from. If the stored session is at a different version, fail — do not
merge, do not overwrite. That single rule is what makes concurrent workers safe.

**One commit, one transaction.** The commit's messages, phase, version and head
land together or not at all. A half-applied turn is a corrupted session.

**A node has a parent.** The tree is what makes branching possible: a commit
names the `NodeId` it continues from. Rewinding is a new commit whose parent is an
older node — the old branch is still there.

**Blobs are content-addressed, `put` is idempotent.** Store them where large
binary data belongs and keep the id in the part.

## Where to put the pieces

| Piece | Fits |
| --- | --- |
| Sessions, nodes, messages | A relational table per column, or one JSON document per node. |
| Media, large tool output | `BlobStore`: a filesystem, S3, or a `BYTEA` table. |

Two indexes earn their keep immediately: `(session, version)` for the CAS check
and `(node, parent)` for ancestors.

## Moving a session between processes

Persistence is not the only way to move a conversation. Export/import is the
portable one:

```rust,ignore
let json = engine.export(session.id).await?.to_json()?;
std::fs::write("session.json", json)?;

// elsewhere
let conversation = Conversation::from_json(&std::fs::read_to_string("session.json")?)?;
let session = other_engine.import(conversation).await?;
```

The document is versioned (`CONVERSATION_SCHEMA_VERSION`) and deterministic. On
the same model, capabilities, tools and parameters, the import reconstructs the
original requests byte-for-byte, which is also what preserves the provider cache
prefix. See [History and durability](../concepts/history-and-durability.md).

## Resuming what a restart interrupted

A session is durable at every commit, so a restart loses at most the turn in
flight. To continue where it stopped:

```rust,ignore
let session = engine.session(session_id).await?;
match session.phase {
    Phase::Idle => { /* start a new turn */ }
    Phase::Streaming | Phase::AwaitingTool => { /* resumable: engine.resume(id) */ }
}
```

`engine.resume(id)` continues the turn. The session is durable at every commit, so
what a restart interrupted is exactly what the phase says it is.
