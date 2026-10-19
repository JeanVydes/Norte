//! An agent turn: the model edits a file through the shipped host tools.
//!
//! The engine runs the loop, not the host. A call the model emits is dispatched
//! to the registry's executor, the result is committed as a tool message, and
//! the model is asked again — until it answers without calling anything. The
//! host here only wires the pieces: the three file executors from
//! `rustychain-tools`, a policy that decides where a path may land, and the
//! model.
//!
//! The policy re-roots the relative paths the prompt names into a scratch
//! directory and refuses anything that tries to leave it. That is a *host*
//! decision about what may be attempted — the executors write where they are
//! told and confine nothing (FR-POL-1) — which is why the directory is printed
//! afterwards: the file on disk is the evidence, not the model's word.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p rustychain-cookbook --example agent
//! ```
//!
//! The prompt asks for one file, one look at it and one change to it, so every
//! call and its result is something to read in the output.

use std::error::Error;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rustychain_core::{
    Capabilities, Decision, Engine, EngineBuilder, InMemoryBlobStore, InMemoryStore, Message,
    Modality, ModelId, ModelRef, Policy, PolicySet, PolicyView, ProviderId, StreamingGranularity,
    ToolKind, ToolRegistry, ToolResultPart, WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};
use rustychain_tools::{EditTool, ReadTool, WriteTool};
use serde_json::Value;

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
async fn turn(engine: &Engine, prompt: &str) -> Result<(), Box<dyn Error>> {
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

/// Re-roots a relative path the model names into `root`, leaving everything
/// else in the call alone: a `Transform` replaces the arguments object, so the
/// fields the tool needs are copied over rather than rebuilt (FR-POL-2).
struct IntoSandbox {
    root: PathBuf,
}

impl Policy for IntoSandbox {
    fn name(&self) -> &'static str {
        "into-sandbox"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        let Some(path) = path_of(view) else {
            return Decision::Allow;
        };
        if escapes(path) {
            // The next policy refuses it; re-rooting an absolute path would
            // silently accept an escape by rewriting it into a different file.
            return Decision::Allow;
        }
        let mut arguments = view.call.arguments.clone();
        let Some(object) = arguments.as_object_mut() else {
            return Decision::Allow;
        };
        let landed = self.root.join(path);
        object.insert(
            "path".to_owned(),
            Value::String(landed.to_string_lossy().into_owned()),
        );
        Decision::Transform(arguments)
    }
}

/// Refuses a path that tries to leave the scratch directory.
struct StayInside {
    root: PathBuf,
}

impl Policy for StayInside {
    fn name(&self) -> &'static str {
        "stay-inside"
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        match path_of(view) {
            Some(path) if escapes(path) => Decision::Deny(format!(
                "{path} is outside {}; every path is relative and lands in the scratch directory",
                self.root.display()
            )),
            _ => Decision::Allow,
        }
    }
}

/// The `path` argument of every file call, whatever else the call carries.
fn path_of<'b>(view: &PolicyView<'b>) -> Option<&'b str> {
    view.call.arguments.get("path").and_then(Value::as_str)
}

fn escapes(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        || path
            .components()
            .any(|component| component == Component::ParentDir)
}

/// A directory of its own, emptied on every run.
fn scratch() -> Result<PathBuf, Box<dyn Error>> {
    let root = std::env::temp_dir().join("rustychain-cookbook-agent");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

/// The wiring: the three file executors, the policy that decides their paths,
/// and the model.
fn engine(root: &Path) -> Result<Engine, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_tool_declaration(ReadTool::new().into_declaration())?;
    tools.register_tool_declaration(WriteTool::new().into_declaration())?;
    tools.register_tool_declaration(EditTool::new().into_declaration())?;

    let policies = PolicySet::new(vec![
        Arc::new(IntoSandbox {
            root: root.to_path_buf(),
        }),
        Arc::new(StayInside {
            root: root.to_path_buf(),
        }),
    ]);

    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build();
    let model = openrouter(capabilities)?;
    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .policies(policies)
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
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4-flash-0731")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities,
    )?)
}
