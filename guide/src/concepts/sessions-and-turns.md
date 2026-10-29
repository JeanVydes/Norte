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
| `metadata` | Free-form `BTreeMap<String, Value>` for the host (observer names land here, for instance). |
| `created_at` / `updated_at` | `DateTime<Utc>`. |

### Phases

`Phase` is the session's state machine, and `Session::phase` is the field that
holds it:

| Phase | Meaning |
| --- | --- |
| `Idle` | Nothing in flight. The only phase in which `append_history` is allowed. |
| `Streaming` | A turn is being produced right now. |
| `AwaitingTool` | The model asked for a tool the engine is about to run. |

`Phase::is_blocked()` is true for `AwaitingTool`, which is how a worker knows a
session it picked up has calls to run rather than a new turn to start.

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
interrupted mid-loop or that still owes its tool calls.

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
| `start()` | Produce the `HarnessStream`. |

### The outcome

`HarnessStream::finish()` returns `TurnOutcome { session, message }`:

- `session` is the session as committed — phase, version, head.
- `message` is `Option<Message>`: the assistant message this turn committed, if
  it committed one. A turn that ends parked at a tool call may commit only a tool
  result.

`HarnessStream` also has `cancel()` and is a `Stream<Item = Result<EngineEvent>>`;
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
- the new phase, version and turn counter.

`InMemoryStore` implements all of this. `CommitSucceeded { session, version }` and
`CommitConflict { session, expected, actual }` are the events to watch.

## Steering

A host that wants to correct an agent mid-loop registers a **turn
interceptor**. The engine consults it before the turn's input, before every
model request and before the turn ends, and commits what it returns, so the
message lands in the history and in the request digest.

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

The consultation points are safe ones: the message arrives before the turn's input or after the
tool results of the previous step, never between a tool call and its result.

Because the turn asks the host before it ends, a message the host was holding
keeps the turn alive: the steering never waits for the next turn.

The interceptor reads the history the request carries, so it can decide on the
conversation itself: the call the model asked for, the tool that answered it,
the instruction the human gave.

A layer is not a substitute. A layer wraps the model call and sees the request
after it was built, so what it adds is neither durable nor part of the history.

## Nothing here is provider-specific

A session records what the model said and what the tools returned. The provider
that produced it is a `ModelRef` on the commit, not a property of the session:
the next turn in the same session may use a different model, and the history
replays as history.
