# Interceptors, and confining a tool

The engine has one seam where the host speaks while a turn runs: the
interceptor. It is consulted before the turn's input, before every model
request, before a tool call, and before the turn ends; it sees the conversation,
and it can inject messages, stop the turn, refuse one call, or step aside.

```rust,ignore
use norte_core::{Message, Result};
use norte_harness::{Interception, InterceptionContext, TurnInterceptor};

#[async_trait::async_trait]
impl TurnInterceptor for BudgetGuard {
    fn name(&self) -> &str {
        "budget-guard"
    }

    async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception> {
        if spent(context.session) > limit() {
            return Ok(Interception::Stop {
                reason: "budget exhausted".to_owned(),
            });
        }
        Ok(Interception::Continue)
    }
}
```

| Variant | Effect |
| --- | --- |
| `Continue` | The turn goes on as it would. |
| `Inject(Vec<Message>)` | These messages join the session before the next request. |
| `Stop { reason }` | The turn ends here, with the host's reason on it. |
| `RefuseCall { reason }` | The call being decided at `ToolCall` does not run; the model reads why. |

An injected message is durable: the engine commits it itself, so it is part of
the request digest, part of the history a replay sees, and free of a version
race, because while a turn runs the engine is the only writer. This is what
makes an interceptor the right place for a steering message that has to reach
the request already in flight — a [layer](layers-and-observers.md) sees the
request after it was built, so what it adds is neither durable nor visible to
anything but that one call.

`InterceptionContext` carries `point` (which question is being asked), `session`,
`turn`, `head`, `messages` (the history the next request will carry) and `events`
(the sink, for a host that wants to say something), plus `call()`, `tool_name()`,
`call_id()` and `arguments()` for the call at `ConsultationPoint::ToolCall`.

## Deciding about one tool call

The engine asks the interceptors at `ConsultationPoint::ToolCall` immediately
before a call runs, when its name and arguments are known and nothing has
happened yet. `Interception::RefuseCall { reason }` answers that question: the
call does not run, the model is handed an error result carrying the reason, and
it reads that exactly like a failed call — the model asked for something, it
gets an answer that says why not, and it can adapt. That is the whole of the
feedback loop, and it works the same for a call that is refused as for one that
failed. The refusal is not silent: the call is reported started and finished as
a failure, so a host that renders the engine's events shows the call it will
never run instead of a gap.

A rule that is about one tool's arguments belongs in that tool's declaration,
where the arguments are typed. Wrapping a declaration is how a host re-roots
paths, pins a working directory, or refuses an argument it will not accept:

```rust,ignore
use std::sync::Arc;
use norte_core::{FnExecutor, Result, ToolArgs, ToolDeclaration, ToolOutput};

struct Sandboxed<A> {
    inner: Arc<dyn FnExecutor<A, ToolOutput>>,
    root: std::path::PathBuf,
}

#[async_trait::async_trait]
impl<A> FnExecutor<A, ToolOutput> for Sandboxed<A>
where
    A: Pathed + ToolArgs,
{
    async fn call(&self, mut args: A) -> Result<ToolOutput> {
        let path = args.path_mut();
        if path.is_absolute() {
            return Err(norte_core::Error::InvalidInput(format!(
                "{} is absolute; every path is relative and lands in {}",
                path.display(),
                self.root.display()
            )));
        }
        *path = self.root.join(&*path);
        self.inner.call(args).await
    }
}
```

The wrapper owns the rule because the rule is about *that* tool's arguments. A
host that needs the conversation to decide — "this session may not spend more
than this", "no tool named in the deny list runs", "not while a background job
is running" — writes an interceptor instead, because that is the seam that sees
the conversation and every call.

`norte-tools` ships the executors, not the policy: `read`, `write` and `edit`
write where they are told, and `shell` runs the script it is given. Confining
them is the host's job, and a wrapper is the smallest way to do it.

## What is not the answer

- **Argument rewriting in the engine.** A rule that lives in the engine has to
  know every tool's schema; a rule that lives in the tool's wrapper already has
  the type.
- **A sandbox by argument inspection.** Confining a path is not the same as
  confining a process. The real containment is the process, the filesystem and
  the user the host runs under, and a rule here decides what may be *attempted*.
- **A gate that hides what it left out.** A refusal is a message to the model:
  say which argument was wrong and what shape would be accepted, or the model
  guesses again, expensively.
