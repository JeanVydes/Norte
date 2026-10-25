# History and durability

## Two ports

Persistence is two traits. The engine depends on nothing else.

```rust,ignore
#[async_trait::async_trait]
pub trait Store {
    async fn commit(&self, commit: Commit) -> Result<Session>;
    async fn load(&self, session: SessionId) -> Result<Session>;
    async fn node(&self, node: NodeId) -> Result<Node>;
    async fn ancestors(&self, node: NodeId) -> Result<Vec<NodeId>>;
    async fn children(&self, node: NodeId) -> Result<Vec<NodeId>>;
    async fn save_approval(&self, record: ApprovalRecord) -> Result<()>;
    async fn approval(&self, id: ApprovalId) -> Result<Option<ApprovalRecord>>;
    async fn approvals(&self, session: SessionId) -> Result<Vec<ApprovalRecord>>;
    // async fn history(&self, head: Option<NodeId>) -> Result<Vec<Message>>;  // default impl
}

#[async_trait::async_trait]
pub trait BlobStore {
    async fn put(&self, bytes: &[u8]) -> Result<BlobId>;
    async fn get(&self, id: BlobId) -> Result<Vec<u8>>;
    async fn contains(&self, id: BlobId) -> Result<bool>;
}
```

`InMemoryStore` and `InMemoryBlobStore` implement both, atomically, with the
optimistic-concurrency check and the approval ledger included. They are not a
mock: they are the right default for a CLI, a desktop app or a single-process
worker. Implement the traits over your own database and object store when a
session has to outlive the process.

## What a commit contains

One commit per turn, carrying:

- the parent `NodeId` (the session's current `head`),
- the messages the turn produced,
- the new phase, version and turn counter,
- the `expected_version` the commit was built from.

The store is compare-and-swap on that version. A stale commit fails; it does not
merge and it does not overwrite. `CommitConflict { expected, actual }` tells the
host what it raced.

`BlobStore` is content-addressed: `put` is idempotent, and the id is a hash of the
bytes. A media part too large to sit inline in the history is stored as a blob
and referenced from the part; `HarnessConfig::inline_threshold` (64 KiB by
default) is where that line falls.

## Export and import

```rust,ignore
let conversation = engine.export(session.id).await?;   // -> Conversation
let json = conversation.to_json()?;                    // a plain, versioned document
// .. later, maybe in another process, maybe on another machine ..
let restored = Conversation::from_json(&json)?;
let session = engine.import(restored).await?;
```

`Conversation` is the session plus its history, in an append-only, deterministic
form with its own `CONVERSATION_SCHEMA_VERSION`. It is meant to be written to a
file, a queue or a database.

**Byte-identity is the claim**, and it has conditions worth stating plainly: an
import reproduces the original request byte-for-byte when the model,
capabilities, tools and generation parameters are the same as they were, and the
history was lossless. Re-importing under the *same* session id preserves the
provider cache prefix, because the reconstructed request is identical.

That is why parts exist. Continuity tokens, provider-executed blocks and
reasoning signatures are kept verbatim precisely so the reconstructed request
does not differ from the original by a byte.

## Approvals survive restarts

`ApprovalRecord`s live in the `Store`, not in memory. A session parked at
`AwaitingApproval` can be picked up by a different process, decided there, and
resumed. `engine.pending_approvals(session)` returns the live ones; the audit
entries stay with the record.

## What a commit does not cover

A commit makes the *conversation* durable; it says nothing about the world
outside it. A committed tool result is never produced twice (FR-ERR-5), but a
tool's external effect — a charge, an email, a write to another system — is not
part of that commit, and the window between the effect and the commit is the
host's to close: a process killed there is handed the same call on `resume`. The
engine gives the tool the stable identity to key on, `session` and the
engine-assigned `call_id` (FR-ID-2), and leaves the record to the host. The
pattern is [`cookbook/idempotent_tool`](https://github.com/JeanVydes/Norte/blob/dev/examples/cookbook/examples/idempotent_tool.rs);
the requirement is FR-ERR-6.

## What is deliberately absent

- **No migrations, no schema, no SQL.** The engine defines the shape of a commit;
  how you store it is yours.
- **No global registry of sessions.** A `Store` can list what it likes; the
  engine only ever loads by id.
- **No implicit retention.** Blobs are referenced by history and never collected
  by the engine. A host that deletes sessions owns the garbage collection.
- **No effect-delivery machinery.** Leases, effect classes and an
  `OutcomeUnknown` state are not in the engine: deciding which effects are
  idempotent, and resolving an ambiguous one, is host policy, like retries and
  rate limits (FR-ERR-4, FR-ERR-6).
