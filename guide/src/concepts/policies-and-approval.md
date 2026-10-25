# Policies and approval

Every tool call the model asks for passes through the policy set before anything
runs. A policy is a small synchronous function over a view of the call, and it
returns a decision.

```rust,ignore
use norte_core::{Decision, Policy, PolicyView};

struct NoSecrets;

impl Policy for NoSecrets {
    fn name(&self) -> &str {
        "no-secrets"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        // view.call is the ToolCallPart the model produced.
        if view.call.name.as_str() == "read" && view.call.arguments.to_string().contains(".env") {
            return Decision::Deny("`.env` is not readable by a model".to_owned());
        }
        Decision::Allow
    }
}
```

## Decision

| Variant | Effect |
| --- | --- |
| `Allow` | Run the call as the model wrote it. |
| `Deny(String)` | Do not run it. The string becomes the error tool result the model reads. |
| `Transform(Value)` | Run it, with these arguments instead. Replace the whole object, so copy the fields you keep. |
| `RequireApproval(String)` | Park the turn and ask a human; the string is the reason. |
| `RateLimit(Duration)` | Not yet: the model is calling too often. |

A `Deny` message is a prompt: it is the only feedback the model gets, so say what
was wrong and what shape would be accepted. A denial that says nothing teaches
nothing, and the model will guess again — an expensively recurring mistake.

## The policy set

`PolicySet::new(vec![Arc::new(p1), Arc::new(p2)])`. Every policy is evaluated, and
the outcomes are combined: a `Deny` wins, then `RequireApproval`, then the last
`Transform`, then `Allow`. The precedence does not depend on registration
order: a `Transform` after a `RequireApproval` cannot erase the approval. When
both apply, the approval carries the last rewritten arguments, so the human
approves exactly what will run. `PolicyView::prior` lets a later policy see what
the earlier ones decided, and `PolicyView::results` shows the tool results
produced so far in this turn, which is how a policy can react to what the
request has already done rather than only to what it asks next.

The view carries: `session`, `model`, `capabilities`, `call` (the
`ToolCallPart`), `message` (the assistant message it came in), `prior`,
`results`, and `history` (the committed messages).

`ToolApprovalPolicy::new(name, tools, reason)` is the ready-made policy that
requires approval for a named set of tools.

## Transform, and sandboxing

`Transform` is how a host re-roots paths, pins a working directory, or narrows a
command without touching the model. `benchmarks/agent-cost` uses three:
re-root every relative path into a scratch directory, refuse a path that escapes
it, and narrow `command` to `program: "cargo"` with a working directory the model
cannot widen.

Two notes from doing exactly that:

- **`Transform` replaces the arguments object.** If the tool has other fields,
  copy them over — a transform that returns `{"path": ..}` alone has dropped
  everything else.
- **A policy is not a sandbox.** It is a gate on arguments. The real containment
  is the process, the filesystem and the user the host runs under.

## Approval

`RequireApproval` parks the turn:

1. The engine persists an `ApprovalRecord` and commits `Phase::AwaitingApproval`.
2. `ApprovalRequested { session, approval, call_id, tool }` is emitted.
3. The turn ends. The process may exit; the session is durable.

The record stores the exact arguments being approved in
`ApprovalRecord::arguments`, after any policy transform, so a host shows what
will run rather than what the model originally wrote. A record written by an
older Norte version has `None` there; fall back to the call in history.

A host then decides, using the recorded id:

```rust,ignore
use norte_core::GrantScope;

let pending = engine.pending_approvals(session.id).await?;
let record = engine
    .approve(approval_id, GrantScope::Tool(ToolName::new("read")?), "jane")
    .await?;
// engine.deny(approval_id, "jane", "not for this task").await?;
// engine.revoke(session.id, grant_id, "jane").await?;
```

Then `engine.resume(session.id)` continues the turn from the durable phase.

### What a grant is bound to

An `ApprovalRecord` stores the `argument_digest` of the call it was requested
for, the arguments themselves in `arguments`, and `authorize(now, arguments)`
checks the arguments against the digest. A grant is therefore for *those*
arguments, not for "edit something". `GrantScope` decides how wide it reaches:

| Scope | Covers |
| --- | --- |
| `Call(CallId)` | The one call. |
| `Tool(ToolName)` | Any call to that tool in the session. |
| `Session(SessionId)` | Any tool in that session. |
| `Global` | Anything this engine is asked about. |

Alongside the grants, the record keeps an audit trail — `Requested`, `Granted`,
`Denied`, `Revoked`, `Edited`, `Expired`, `Replayed` — so what was approved, by
whom and when is in the store, not only in a log.

`HarnessConfig::approval_ttl` (ten minutes by default) is how long a pending
request lives.
