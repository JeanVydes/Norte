# Tools

A tool is a name, a description, a JSON Schema and something that runs when the
model calls it. The engine does not know what a tool *means*: it shows the
declaration to the model, and when the model calls it, it asks the policies, runs
the executor, and commits the result as a `ToolResultPart`.

## The registry

```rust,ignore
use rustychain_core::ToolRegistry;
use rustychain_tools::{EditTool, ReadTool};

let mut tools = ToolRegistry::new();
tools.register_tool_declaration(ReadTool::new().into_declaration())?;
tools.register_tool_declaration(EditTool::new().into_declaration())?;
```

| Method | For |
| --- | --- |
| `register_tool_declaration(ToolDeclaration<A>)` | A tool that returns `ToolOutput` (media, structured data, errors). |
| `register_declaration(FunctionDeclaration<A, R>)` | A tool whose return type is the result (`R: Into<ToolOutput>`). |
| `register_declarator(&D)` | A `#[tool]` struct, declared with the plain return type. |
| `register_tool(&D)` | A `#[tool]` struct, returning `ToolOutput`. |
| `register(Arc<dyn AnyFunction>)` | An erased tool, e.g. one wrapping an MCP server. |
| `with_tools(impl IntoIterator<Item = Arc<dyn AnyFunction>>)` | Build a registry in one call. |

Reading it back: `get(&ToolName)`, `spec(&ToolName)`, `specs()`, `len()`,
`is_empty()`, and `execute(&ToolContext, &ToolName, Value)`.

## Declaring one

Three levels, from most fluent to most explicit.

**The macro**, on a struct that names the tool and an executor that runs it:

```rust,ignore
use rustychain_core::{FnExecutor, Result, ToolOutput};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct EchoArgs {
    text: String,
}

#[rustychain_core::tool(name = "echo", description = "Repeat the text back.")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, ToolOutput> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<ToolOutput> {
        Ok(ToolOutput::text(args.text))
    }
}

tools.register_tool(&Echo)?;         // or `register_declarator` for a plain return type
```

The macro derives `Clone` and implements `ToolMetadata` (`NAME`,
`DESCRIPTION`) from the attributes; the executor supplies the behaviour, and the
blanket declarators fill in the schema from `EchoArgs` — which is why the
argument type has to be `JsonSchema + Serialize + DeserializeOwned`.

**The declaration constructors**, when you want the schema derived from a type
with `schemars`:

```rust,ignore
use rustychain_core::ToolDeclaration;

let declaration = ToolDeclaration::new(
    "read",
    "Read a file, optionally a line range or a pattern.",
    executor,                    // the eventual `FnExecutor`
);
```

`FunctionDeclaration::new(..)` is the same for a typed return, and both have
`.with_approval(reason)` to mark the tool as one a human must approve by default.

**`declare_function!` / `declare_tool!`** are the macro forms over an existing
`async fn`, when the function signature already describes the schema.

## The schema the model sees

`ToolSpec::provider_declaration(model, wire)` renders a spec for a specific wire.
Two rules apply (and are pinned by tests in the repository):

- **Self-contained schemas.** `schemars` emits `$defs` plus `$ref` for nested
  argument types, and models that do not follow references answer with the wrong
  JSON kind — a string where the object belongs. The declaration is expanded
  before it reaches a wire; unexpandable references keep their definitions so the
  document stays valid.
- **Tags are written as discriminators.** An enum that serde tags
  (`#[serde(tag = "kind")]`) must also derive `#[schemars(tag = "kind")]`, so
  each variant names its tag as a `const` in the schema. Without it the model sees
  a bare `oneOf` and guesses — the failure that cost the `command` tool two to
  seven extra requests per task in the cost benchmark.

`ToolSpec.description` is the text the model reads for the tool itself, and it is
an explicit argument of the declaration — never inherited from a doc comment.
Field descriptions are a different matter: with `schemars`, a `///` on an argument
field *becomes* that field's description in the schema, so it is model-facing too.
Write both for a model, and keep internal requirement codes and rustdoc links out
of them — a test in this repository fails if one reaches the wire.

## Shipped executors

`rustychain-tools` ships four, each with `new()`, `with_limits(..)` and
`into_declaration()`:

| Tool | Arguments | Notes |
| --- | --- | --- |
| `read` | `path`, plus `lines`, `tail`, `pattern`, `context`, `invert`, `ignore_case`, `count_only`, `max_matches` | Reads files or searches them. |
| `write` | `path`, `content`, `overwrite` | Creates or replaces a whole file. |
| `edit` | `path`, `edits: [{search, replace}, ..]` | Exact-match, atomic, with a failure report that says what did not match. |
| `command` | `command: {kind: "program"|"shell", ..}`, plus `cwd`, `env`, `timeout_ms`, `max_output_bytes`, `stdin`, .. | Runs a program or a shell script. |

`CommandTool` has two builder methods worth knowing:

- `with_env(BTreeMap<String, String>)` — the executor clears the environment
  (`env_clear()`) and runs the child with exactly what you pass, so a process
  that needs `PATH` to find `cargo` gets it from here and nowhere else.
- `with_secrets(impl IntoIterator<Item = impl Into<String>>)` — values redacted
  from the tool result before it is shown to the model.

A `command` request names any program. **Authorizing it is the host's job.** The
executor will run what it is given; the policy is what decides whether `cargo`
yes and `rm` no. See [Policies and approval](concepts/policies-and-approval.md).

## Results

`ToolOutput` is what an executor returns:

```rust,ignore
ToolOutput::text("compiled in 3.2s");          // a text part
ToolOutput::json(json!({ "hits": 12 }));       // structured, and readable
ToolOutput::error("no such file");             // is_error: true
ToolOutput::text("...").with_media(media);     // text plus media parts
```

The engine turns it into a `ToolResultPart { call_id, parts, structured,
is_error }` and commits it. A failed tool is data the model reads, not an engine
error: the next request contains the failure and the turn continues.

## Context

`ToolContext` is handed to an executor and carries the session, the call id, a
cancellation token, a progress sink and free-form metadata. It is how a long
tool cancels cleanly when the turn is cancelled.
