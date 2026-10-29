//! An agent turn: the model edits a file through the shipped host tools.
//!
//! The engine runs the loop, not the host. A call the model emits is dispatched
//! to the registry's executor, the result is committed as a tool message, and
//! the model is asked again — until it answers without calling anything. The
//! host here only wires the pieces: the three file executors from
//! `norte-tools`, wrapped so a path may only land in the scratch directory, and
//! the model.
//!
//! The wrapper re-roots the relative paths the prompt names into a scratch
//! directory and refuses anything that tries to leave it. That is a *host*
//! decision about what may be attempted — the executors write where they are
//! told and confine nothing — which is why the directory is printed afterwards:
//! the file on disk is the evidence, not the model's word.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example agent
//! ```
//!
//! The prompt asks for one file, one look at it and one change to it, so every
//! call and its result is something to read in the output.

use std::error::Error;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use norte_core::{
    Capabilities, FnExecutor, Message, Modality, ModelRef, Result as NorteResult,
    StreamingGranularity, ToolArgs, ToolDeclaration, ToolKind, ToolOutput, ToolRegistry,
    ToolResultPart, WireFamily,
};
use norte_harness::{Harness, HarnessBuilder, InMemoryBlobStore, InMemoryStore};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use norte_tools::{EditRequest, EditTool, ReadRequest, ReadTool, WriteRequest, WriteTool};

const SYSTEM: &str = "\
You are a coding agent working in a scratch directory. Use the tools, do not \
describe what you would do. The workflow is exactly: (1) call `write` to create \
`notes.md` with exactly three lines — `alpha`, `beta`, `gamma` — one per line, \
ending with a newline; (2) call `read` on `notes.md` to see it; (3) call `edit` \
on `notes.md` to replace the line `beta` with the line `BETA`; (4) answer with \
the three lines of the file as they now are, and nothing else. Every path is \
relative: `notes.md`.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let root = scratch()?;
    println!("scratch directory: {}\n", root.display());
    let engine = engine(&root)?;

    println!("--- the loop: create, look, change, report");
    turn(
        &engine,
        "Create the file, look at it, change beta to BETA, and report the file.",
    )
    .await?;

    println!("\n--- a path that tries to leave the sandbox");
    turn(&engine, "Read /etc/hostname and report its first line.").await?;

    println!("\n--- the directory, which is the evidence");
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        print!("{}", path.display());
        if let Ok(text) = std::fs::read_to_string(&path) {
            println!(":\n{text}");
        } else {
            println!(" (not text)");
        }
    }
    Ok(())
}

/// One agent turn of its own: a session, the system prompt, and everything the
/// engine did before it answered.
async fn turn(engine: &Harness, prompt: &str) -> Result<(), Box<dyn Error>> {
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(SYSTEM)])
        .await?;
    let outcome = engine
        .send(session.id, Message::user(prompt))
        .finish()
        .await?;

    for message in engine.history(session.id).await? {
        for call in message.tool_calls() {
            println!("call   {}({})", call.name, call.arguments);
        }
        for result in message.tool_results() {
            println!("result {}", line(result));
        }
    }
    println!("answer {}", answer(outcome.message.as_ref()));
    Ok(())
}

/// The first line of a tool result, which is where the shipped executors put
/// their summary (FR-TOOL-FILE-3).
fn line(result: &ToolResultPart) -> String {
    let status = if result.is_error { "failed: " } else { "" };
    let text = result
        .parts
        .iter()
        .find_map(|part| part.as_text())
        .unwrap_or_default();
    format!("{status}{}", text.lines().next().unwrap_or_default())
}

fn answer(message: Option<&Message>) -> String {
    message.map(Message::text).unwrap_or_default()
}

/// A file request with a path the host may re-root.
trait Pathed {
    /// The path the call names.
    fn path_mut(&mut self) -> &mut PathBuf;
}

impl Pathed for ReadRequest {
    fn path_mut(&mut self) -> &mut PathBuf {
        &mut self.path
    }
}

impl Pathed for WriteRequest {
    fn path_mut(&mut self) -> &mut PathBuf {
        &mut self.path
    }
}

impl Pathed for EditRequest {
    fn path_mut(&mut self) -> &mut PathBuf {
        &mut self.path
    }
}

/// A file executor that lands every path it is given under `root`.
///
/// The host confines a path where the path is read, not in a rule the engine
/// evaluates before the call: the argument is typed here, which is where a
/// rejection can say which field it was about, and what comes back is a tool
/// result the model reads like any other. The engine's own seam for a host
/// decision — the interceptor — sees the conversation, not one tool's
/// arguments, so a per-call rule belongs to the call.
struct Sandboxed<A> {
    inner: Arc<dyn FnExecutor<A, ToolOutput>>,
    root: PathBuf,
}

#[async_trait]
impl<A> FnExecutor<A, ToolOutput> for Sandboxed<A>
where
    A: Pathed + ToolArgs,
{
    async fn call(&self, mut args: A) -> NorteResult<ToolOutput> {
        let path = args.path_mut();
        if escapes(path) {
            return Err(norte_core::Error::InvalidInput(format!(
                "{} is outside {}; every path is relative and lands in the scratch directory",
                path.display(),
                self.root.display()
            )));
        }
        *path = self.root.join(&*path);
        self.inner.call(args).await
    }
}

/// The same declaration, with its paths confined to `root`.
fn sandbox<A>(declaration: ToolDeclaration<A>, root: &Path) -> ToolDeclaration<A>
where
    A: Pathed + ToolArgs + serde::de::DeserializeOwned + std::fmt::Debug + 'static,
{
    ToolDeclaration {
        name: declaration.name,
        description: declaration.description,
        parameters: declaration.parameters,
        executor: Arc::new(Sandboxed {
            inner: declaration.executor,
            root: root.to_path_buf(),
        }),
    }
}

fn escapes(path: &Path) -> bool {
    path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
}

/// A directory of its own, emptied on every run.
fn scratch() -> Result<PathBuf, Box<dyn Error>> {
    let root = std::env::temp_dir().join("norte-cookbook-agent");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// The wiring: the three file executors, confined to `root`, and the model.
fn engine(root: &Path) -> Result<Harness, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_tool_declaration(sandbox(ReadTool::new().into_declaration(), root))?;
    tools.register_tool_declaration(sandbox(WriteTool::new().into_declaration(), root))?;
    tools.register_tool_declaration(sandbox(EditTool::new().into_declaration(), root))?;

    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build();
    let model = openrouter(capabilities)?;
    Ok(HarnessBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .build()?)
}

/// A value the run has to be given: nothing external is defaulted in the
/// source, because a model id or an endpoint written here is a pointer that
/// rots, and a run that quietly picks one is a run nobody chose.
fn required(name: &str, example: &str) -> Result<String, Box<dyn Error>> {
    std::env::var(name).map_err(|_| format!("set {name}, for example {name}={example}").into())
}

/// Wires `OpenRouter`. An `OpenAI`-compatible vendor is a profile, not a preset.
fn openrouter(capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    Ok(OpenAiChat::new(
        ModelRef::new(
            "openrouter".try_into()?,
            required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?
                .as_str()
                .try_into()?,
        ),
        VendorProfile::new(
            "openrouter".try_into()?,
            WireFamily::new(WIRE_CHAT)?,
            &required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?,
        )?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?)),
        capabilities,
    )?)
}
