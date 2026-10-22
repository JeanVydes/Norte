# Sessions and turns

## A session

A `Session` is the durable unit. It carries:

| Field | What it is |
| --- | --- |
| `id` | `SessionId` — a UUID. Stable for the life of the conversation. |
| `head` | `Option<NodeId>` — the tip of the commit tree every turn appends to. |
| `phase` | `Phase` — where the session currently is. |
| `version` | `u64` — bumped by every commit; the optimistic-concurrency token. |
| `turn` | `u64` — how many turns have run. |
| `queue` | `VecDeque<Message>` — messages enqueued by `steer`, applied at the next turn boundary. |
| `grants` | `Vec<Grant>` — live tool approvals. |
| `metadata` | Free-form `BTreeMap<String, Value>` for the host (observer names land here, for instance). |
| `created_at` / `updated_at` | `DateTime<Utc>`. |

### Phases

`Phase` is the session's state machine, and `Session::phase` is the field that
holds it:

| Phase | Meaning |
| --- | --- |
| `Idle` | Nothing in flight. The only phase in which `append_history` is allowed. |
| `Streaming` | A turn is being produced right now. |
| `AwaitingTool` | The model asked for a tool and a grant authorises it; the engine is about to run it. |
| `AwaitingApproval` | The model asked for a tool and a policy said a human has to say yes. The turn is parked here and the process may end. |

`Phase::is_blocked()` is true for the two waiting phases, which is how a worker
knows a session it picked up needs a decision rather than a new turn.

## A turn

A turn is one round of "send something to the model and commit what it said".
It is opened with `engine.turn(session)` and configured fluently:

```rust,ignore
let mut stream = engine
    .turn(session.id)
    .input(Message::user("Summarise src/lib.rs"))   // optional
    .temperature(0.2)                               // any GenerationParams field
    .tools([ToolName::new("read")?])                // expose a subset of the registry
    .extension("openai.chat:provider", json!({ "only": ["DeepInfra"] }))
    .start();
```

`engine.send(session, message)` is the shorthand for
`turn(session).input(message).start()`, and `engine.resume(session)` is
`turn(session).start()` with no input — used to continue a session that was
parked at `AwaitingApproval` or interrupted mid-loop.

### The turn builder

| Method | Effect |
| --- | --- |
| `input(Message)` | The message that starts the turn. Omitted when resuming. |
| `options(TurnOptions)` | Params, reasoning, tool subset and vendor extensions at once. |
| `params(GenerationParams)` | The generation parameters for this turn. |
| `temperature`, `top_p`, `top_k`, `max_output_tokens`, `stop_sequences`, `seed` | Shorthands for the fields above. |
| `logprobs(u8)` | Ask the model for token probabilities, up to 20 runner-ups. Needs `Feature::Logprobs` on the model. |
| `output_schema(Value)` | A JSON Schema the answer must satisfy. |
| `reasoning(ReasoningRequest)` | Ask for reasoning, and how much of it. |
| `tools(impl IntoIterator<Item = ToolName>)` | Restrict the request to a subset of the registered tools. |
| `extension(key, value)` | A vendor-specific knob, passed through to the wire. |
| `start()` | Produce the `EngineStream`. |

### The outcome

`EngineStream::finish()` returns `TurnOutcome { session, message }`:

- `session` is the session as committed — phase, version, head.
- `message` is `Option<Message>`: the assistant message this turn committed, if
  it committed one. A turn that ends parked at a tool call may commit only a tool
  result.

`EngineStream` also has `cancel()` and is a `Stream<Item = Result<EngineEvent>>`;
dropping it cancels. See [Streaming](streaming.md).

## Commits

Every turn ends in exactly one `Store::commit`, and the store is
compare-and-swap: a commit carries the `expected_version` it was built from. If
the stored session has moved on, the commit fails with a `CommitConflict` (or an
`Error`) rather than overwriting someone else's turn. That is what makes two
workers safe to run against one session.

What a commit writes:

- a new `NodeId` whose parent is the current `head`;
- the messages produced in the turn (assistant text, tool calls, tool results);
- the new phase, version and turn counter;
- any `ApprovalRecord` the turn created.

`InMemoryStore` implements all of this. `CommitSucceeded { session, version }` and
`CommitConflict { session, expected, actual }` are the events to watch.

## Steering

`engine.steer(session, message)` enqueues a message without starting a turn. It is
applied at the next turn boundary, inside the same turn, which is how a UI can
correct an agent mid-loop without racing it. The queue is visible as
`Session::queue` and is part of the session that gets committed.

The queue is the durable path: it survives a restart, and a host that wants
the message to reach the request that is already running registers a turn
interceptor instead.

## Reaching the request in flight

The queue is a session field, so a message written to it while a turn runs
cannot be committed without racing that turn. A host that must reach the
request already in flight registers a **turn interceptor** instead:

```rust
#[async_trait::async_trait]
impl TurnInterceptor for Steer {
    fn name(&self) -> &'static str {
        "steer"
    }

    async fn intercept(&self, _context: &InterceptionContext<'_>) -> Result<Interception> {
        let messages = self.take();
        if messages.is_empty() {
            Ok(Interception::Continue)
        } else {
            Ok(Interception::Inject(messages))
        }
    }
}
```

The engine consults it before the turn's input, before every model request and
before the turn ends, and commits what it returns, so the message lands in the
history, in the request digest and in what the policies read. The consultation
points are safe ones: the message arrives before the turn's input or after the
tool results of the previous step, never between a tool call and its result.

Because the turn asks the host before it ends, a message the host was holding
keeps the turn alive: the steering never waits for the next turn.

The interceptor reads the history the request carries, so it can decide on the
conversation itself: the call the model asked for, the tool that answered it,
the instruction the human gave.

A layer is not a substitute. A layer wraps the model call and sees the request
after it was built, so what it adds is neither durable nor visible to the
policies.

## Nothing here is provider-specific

A session records what the model said and what the tools returned. The provider
that produced it is a `ModelRef` on the commit, not a property of the session:
the next turn in the same session may use a different model, and the history
replays as history.
