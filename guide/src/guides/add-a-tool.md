# Add a tool

You are going to add a tool the model can call, register it, gate it with a
policy, and test it without a network.

## 1. Declare it

The smallest complete tools are a struct plus an executor. The struct carries the
name and the description the model sees; the executor says what runs.

```rust,ignore
use norte_core::{FnExecutor, Result, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The text to count. Words are separated by whitespace.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct WordCountArgs {
    text: String,
}

#[norte_core::tool(
    name = "word_count",
    description = "Count the words in text, for example {\"text\": \"the sky is blue\"}."
)]
struct WordCount;

#[async_trait::async_trait]
impl FnExecutor<WordCountArgs, ToolOutput> for WordCount {
    async fn call(&self, args: WordCountArgs) -> Result<ToolOutput> {
        Ok(ToolOutput::json(serde_json::json!({
            "words": args.text.split_whitespace().count(),
        })))
    }
}
```

What each piece requires:

- **The argument type must be `ToolArgs`** — `JsonSchema + Serialize +
  DeserializeOwned + Debug + Send + Sync` — because it is both shown to the model
  as a schema and parsed from what it sends.
- **The macro derives `Clone`** and implements `ToolMetadata` (`NAME`,
  `DESCRIPTION`) from its attributes. With an `FnExecutor` present, the blanket
  declarators apply, so a registration never restates the argument and result
  types.
- **`FnExecutor::call` returns `Result<R>`.** Use `ToolOutput` as `R` when the
  result needs media, structured data or the error flag; any `Serialize` type
  works when it does not.
- **`call_with_context(&ToolContext, args)`** is there when a tool needs the
  session, a cancellation token or a progress sink; the default forwards to
  `call`.

If you would rather build the declaration by hand — parsing arguments yourself,
or supplying a schema you wrote — use one directly:

```rust,ignore
use norte_core::ToolDeclaration;

let declaration = ToolDeclaration::new("word_count", "Count the words in text.", executor);
tools.register_tool_declaration(declaration)?;
```

`FunctionDeclaration::new(name, description, executor)` is the same shape for a
typed return.

## 2. Register it

```rust,ignore
use norte_core::ToolRegistry;

let mut tools = ToolRegistry::new();
tools.register_tool_declaration(declaration)?;
tools.register_tool(&WordCount)?;   // register_declarator if `call` returns a plain result

let engine = HarnessBuilder::new()
    .store(store)
    .blobs(blobs)
    .model(model)
    .tools(tools)
    .build()?;
```

Registered tools are shown to the model on every request they are exposed in.
To expose a subset for one turn: `.tools([ToolName::new("word_count")?])` on the
turn builder.

## 3. Write the description for a model, not for a reader

The tool's own description is an explicit argument — the macro's `description`,
or `ToolDeclaration::new` — and it is a prompt. Three things pay for themselves:

- **Say what the arguments are**, and which are optional.
- **Give one example of the argument object.** Models copy shapes; a single
  example removes most wrong-shape calls.
- **Keep internal codes out of it.** A requirement id or a rustdoc link means
  nothing to a model; a test in this repository fails if one reaches the wire.

Anything the schema carries is model-facing too, so write it for the model. With
`schemars`, a field's `///` doc comment becomes its description:

```rust,ignore
#[derive(JsonSchema)]
struct EditArgs {
    /// The file to change, relative to the project root.
    path: String,
    /// Pairs of exact text to find and the text to put in its place.
    edits: Vec<EditPair>,
}
```

If the arguments contain an enum tagged with serde, tag the schema too
(`#[schemars(tag = "kind")]`), or the model sees a bare `oneOf` and guesses at the
discriminator. This is not hypothetical: the `command` tool uses that shape, and
`benchmarks/schema-friction` has a `tagged` row that exercises it against a live
model. The report's rejected-call count is the measurement; the guide does not
promise a fixed number of extra requests.

## 4. Gate it

A tool that touches the world gets a policy:

```rust,ignore
use norte_harness::{Decision, Policy, PolicySet, PolicyView};

struct WordCountIsHarmless;

impl Policy for WordCountIsHarmless {
    fn name(&self) -> &'static str {
        "word-count-is-harmless"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        if view.call.name.as_str() == "word_count" && view.call.arguments["text"].as_str().is_none()
        {
            return Decision::Deny(
                "`text` must be a string, for example {\"text\": \"the sky is blue\"}".to_owned(),
            );
        }
        Decision::Allow
    }
}

let engine = HarnessBuilder::new()
    // ..
    .policies(PolicySet::new(vec![std::sync::Arc::new(WordCountIsHarmless)]))
    .build()?;
```

A `Deny` message is read by the model. Say what was wrong *and* what shape would
be accepted, or the model will guess again — and every guess is a billed request.

## 5. Test it without a network

```rust,ignore
use norte_testing::{model, ScriptedModel, text_capabilities};

let scripted = ScriptedModel::new(model("test/tool-model"))
    .with_capabilities(text_capabilities())
    .turn_tool_call(call_id, "word_count", &json!({ "text": "the sky is blue" }))
    .turn_text("Three words.");

// .. run a turn against `scripted` ..
assert_eq!(scripted.request_count(), 2);
```

`ScriptedModel` answers from a script, `ProbeModel` records the requests that went
through it, and `RecordingObserver` collects the events. See
[Test without a network](test-without-a-network.md).
