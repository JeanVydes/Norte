# Messages and parts

## Message and Role

A `Message` is a `Role` plus a list of `Part`s, plus an optional origin.

```rust,ignore
use norte_core::{Message, Part};

let system = Message::system("You are a coding agent.");       // Role::System
let user = Message::user("What does src/lib.rs export?");      // Role::User
let assistant = Message::assistant("It exports `Config`.");    // Role::Assistant
```

| Constructor | Role |
| --- | --- |
| `Message::system(text)` | `System` |
| `Message::developer(text)` | `Developer` |
| `Message::user(text)` | `User` |
| `Message::assistant(text)` | `Assistant` |
| `Message::tool(results)` | `Tool`, from `Vec<ToolResultPart>` |
| `Message::new(role, parts)` | Anything, when you have the parts already |

`From<&str>` and `From<String>` produce a user message. `origin` is how the
engine records what produced the message (provider, model, `Usage`), and `cache`
is the `CacheClass` that marks where a provider cache boundary may fall.

Accessors: `text()` (all text parts concatenated), `reasoning()`,
`tool_calls()`, `tool_results()`, `media()`, `has_tool_calls()`,
`finish_reason()`.

## Part

`Part` is a tagged enum — `{"type": "text", ..}` on the wire form — and it is
`#[non_exhaustive]`, so match it with a `_` arm:

| Variant | Holds |
| --- | --- |
| `Text(TextPart)` | `text`, `citations`, and a `continuity` token (thought signatures, encrypted reasoning). |
| `Reasoning(ReasoningPart)` | `text` and a `ReasoningVisibility`: `Full`, `Summary`, `Redacted` or `Omitted`. |
| `Media(MediaPart)` | Attached or produced media. |
| `ToolCall(ToolCallPart)` | `call_id`, `name`, `arguments: Value`. |
| `ToolResult(ToolResultPart)` | `call_id`, `parts`, optional `structured: Value`, `is_error`. |
| `ProviderToolCall(..)` / `ProviderToolResult(..)` | A block a provider executed on its own side (server-side search, code execution, MCP). Kept, replayed, not interpreted. |
| `Refusal(RefusalPart)` | A refusal, as its own part rather than a magic string. |
| `Extension(ExtensionPart)` | An unknown block, kept verbatim under a namespace. |

Helpers: `Part::text(s)`, `as_text()`, `as_tool_call()`, `as_tool_result()`,
`as_media()`, `continuity()`, `without_continuity()`.

### Why parts, and not strings

A coding agent's transcript is not a string. It is text interleaved with
reasoning blocks, tool calls, tool results and provider-executed steps, and each
one has to be replayable *unchanged* — a provider that hands back a signature has
to get it back exactly, or it will refuse the next request. Modelling that as a
list of typed parts is what makes byte-identical export and re-import possible
(see [History and durability](concepts/history-and-durability.md)).

## Tool calls and results

A `ToolCallPart` is what the model asked for; a `ToolResultPart` is what came
back. They are linked by `call_id`, and the result's `parts` are themselves
`Part`s, so a result can carry text, media or structured JSON:

```rust,ignore
use norte_core::{CallId, Part, ToolResultPart};

let result = ToolResultPart::text(call_id, "fn head_words(text: &str, limit: usize)");
let failure = ToolResultPart::error(call_id, "no such file: src/words.rs");
```

A failed tool is a `ToolResultPart` with `is_error: true` and a message the model
can read. It is not an engine error: the turn continues and the model gets a
chance to fix its own mistake.

## Usage lives on the origin, not the part

`Message.origin` carries a `Usage` — the token counters for the request that
produced the message. `engine::usage_of(&Message)` returns it without unwrapping
the `Option`. See [Usage and cost](concepts/usage-and-cost.md).

## Non-exhaustiveness

`Part`, `StreamDelta`, `EngineEvent` and several enums are `#[non_exhaustive]`:
a new variant is a minor release, so match with `_` and your code keeps
compiling.
