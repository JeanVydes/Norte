# Test without a network

Tests in this repository are hermetic by default: no keys, no network, no
provider. Live calls are `#[ignore]`d and run on a schedule. The crate that makes
that possible is `norte-testing`.

## A scripted model

`ScriptedModel` answers turns from a script you write, so a test says exactly what
the model did:

```rust,ignore
use norte_core::{CallId, Message};
use norte_testing::{model, ScriptedModel, text_capabilities};

let scripted = ScriptedModel::new(model("test/scripted"))
    .with_capabilities(text_capabilities())
    .turn_tool_call(call_id, "read", &json!({ "path": "src/lib.rs" }))
    .turn_text("It exports `Config`.");

let engine = HarnessBuilder::new()
    .store(Arc::new(InMemoryStore::new()))
    .blobs(Arc::new(InMemoryBlobStore::new()))
    .model(Arc::new(scripted.clone()))
    .tools(tools)
    .build()?;

let session = engine.create().await?;
let outcome = engine.send(session.id, Message::user("What does it export?")).finish().await?;

assert_eq!(outcome.message.unwrap().text(), "It exports `Config`.");
assert_eq!(scripted.request_count(), 2);   // the tool round, then the answer
```

| Method | For |
| --- | --- |
| `turn(Vec<StreamEvent>)` | An exact sequence, when you need the wire shape. |
| `turn_text(impl Into<String>)` | A plain text answer. |
| `turn_tool_call(call_id, name, &Value)` | A tool call with arguments. |
| `turn_deltas(iter)` | The same text, split into streaming chunks. |
| `turn_delayed(events, Duration)` | Slow answers, for timeout paths. |
| `requests()` / `request_count()` / `remaining_turns()` | Assert what the engine asked and how much script is left. |

`FailingModel::new(reference, message)` is the model that always fails, for the
error paths. `text_capabilities()` is the common declaration.

## Assert what the engine sent

Wrap any model in `ProbeModel` and the requests are recorded:

```rust,ignore
let probe = Arc::new(ProbeModel::new(scripted));
// .. build the engine with `probe.clone()` ..
let last = probe.last_request().expect("a request");
assert_eq!(last.tools[0].name.as_str(), "read");
assert!(last.messages.iter().any(|m| m.text().contains("What does it export?")));
```

`ModelRequest` is the fully assembled request — the messages, the tool
declarations, the parameters. This is how you test a policy by its effect (the
arguments that reached the wire), a layer, or a prompt change.

## Collect the events

`RecordingObserver::new("test")` implements `Observer` and keeps every
`EngineEvent`, so a test can assert on the lifecycle rather than on the outcome:

```rust,ignore
let observer = Arc::new(RecordingObserver::new("test"));
// .. .observers(vec![observer.clone()]) ..
assert!(observer.events().iter().any(|e| matches!(e, EngineEvent::CommitSucceeded { .. })));
assert_eq!(observer.count(), /* .. */);
```

## Conformance suites

If you implement a `Model` or a `Store`, two suites exist so you do not write the
same tests twice:

```rust,ignore
use norte_testing::{model_conformance, store_conformance};

#[tokio::test]
async fn my_store_is_a_store() {
    store_conformance(&MyStore::new()).await;
}
```

`model_conformance(&model, &expected)` and `store_conformance(&store)` cover the
contracts: the events a turn must emit, the commit rules, the approval ledger.

## Fixtures and the fixtures helper

`norte_testing::{model, user, system}` are small constructors for the common
shapes (`ModelRef`, user and system messages) so a test's setup stays a line long.

## Live tests

Anything that talks to a real provider belongs in an `#[ignore]`d test, gated on a
key, so it runs when you ask for it and never by accident:

```rust,ignore
#[tokio::test]
#[ignore = "calls a real provider; set OPENROUTER_API_KEY"]
async fn answers_over_the_real_wire() {
    let Ok(key) = std::env::var("OPENROUTER_API_KEY") else {
        eprintln!("skipping: no OPENROUTER_API_KEY");
        return;
    };
    // .. a small turn, against the real endpoint ..
}
```

```sh
cargo test --workspace                       # hermetic
cargo test -p norte-testing --test live -- --ignored --nocapture   # live
```

The CI runs the first everywhere and the second only on a schedule or on demand,
with the keys from secrets.

## Benchmarks are the third thing

A benchmark calls a paid model on purpose and is *not* a test: it is never run by
`cargo test`. Each one lives under `benchmarks/`, has its own README, and writes
the result of each run into its own `results/` folder. The rule of that folder is
that a result is produced by the thing that measured it, never written by hand.
