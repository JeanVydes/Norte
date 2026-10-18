//! A streaming `ReAct` agent on `RustyChain`, wired to `OpenRouter`.
//!
//! The agent runs a shell command through an approval-gated tool, fetches the
//! `models.dev` catalogue with a tool of its own, and then hands the result to
//! two specialist models — one for providers, one for models.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p react-agent
//! ```

mod branch;
mod catalog;
mod render;
mod tools;

use std::error::Error;
use std::sync::Arc;

use futures_util::StreamExt;
use rustychain_core::{
    Capabilities, Engine, EngineBuilder, EngineStream, GrantScope, InMemoryBlobStore,
    InMemoryStore, Message, Modality, ModelId, ModelRef, Phase, PolicySet, ProviderId, SessionId,
    StreamingGranularity, ToolApprovalPolicy, ToolKind, ToolName, ToolRegistry, TurnOutcome,
    WireFamily,
};
use rustychain_http::{AuthScheme, VendorProfile};
use rustychain_openai::{OpenAiChat, WIRE_CHAT};
use rustychain_tools::CommandTool;
use serde_json::Value;

use crate::catalog::Cache;
use crate::tools::FetchCatalog;

const OPENROUTER_URL: &str = "https://openrouter.ai/api/v1";

const DEFAULT_AGENT_MODEL: &str = "nvidia/nemotron-3.5-super-vl:free";
const DEFAULT_PROVIDERS_MODEL: &str = "nvidia/nemotron-3-super-120b-a12b:free";
const DEFAULT_MODELS_MODEL: &str = "google/gemma-4-31b-it:free";

/// How many approval rounds we are willing to serve in one run.
const MAX_APPROVALS: usize = 8;

const AGENT_SYSTEM: &str = "\
You are a terse, shell-capable research agent. Use the tools you are given when \
they help, never invent tool output, and report only what you observed.";

const MISSION: &str = "\
Use your tools in order. First, run a shell script that prints the working \
directory and lists the files in it — the `command` tool spells a shell \
invocation as {\"kind\": \"shell\", \"shell\": \"/bin/sh\", \"script\": \"...\"}. \
Then fetch the models.dev catalogue. Finish with a three-sentence report of what \
you found. Do not call a tool twice.";

const PROVIDERS_SYSTEM: &str = "\
You are a provider analyst. Given a JSON list of AI providers from models.dev, \
write an opinionated briefing of at most six bullets: which providers matter and \
why, who has the broadest coverage, and who is redundant. Name providers.";

const MODELS_SYSTEM: &str = "\
You are a model analyst. Given a JSON list of AI models from models.dev, write an \
opinionated briefing of at most six bullets on the state of the landscape: \
reasoning, tool-calling and vision coverage, the notable families, and the gaps. \
Name models.";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let key = std::env::var("OPENROUTER_API_KEY")?;
    let cache = Cache::shared();
    let engine = agent_engine(
        &key,
        &env_or("AGENT_MODEL", DEFAULT_AGENT_MODEL),
        &env_or("CATALOG_URL", catalog::DEFAULT_URL),
        cache.clone(),
    )?;

    let session = engine.create().await?;
    run_agent(&engine, session.id).await?;

    let catalog = cache
        .get()
        .ok_or_else(|| std::io::Error::other("the agent never fetched the catalogue"))?;
    let (providers, models) = catalog::counts(&catalog);
    render::section(
        &format!("catalogue: {providers} providers, {models} models"),
        render::GREEN,
    );

    fan_out(&key, &catalog).await
}

/// Runs the `ReAct` turn to completion, asking the operator about every gated
/// tool call along the way.
async fn run_agent(engine: &Engine, session: SessionId) -> Result<(), Box<dyn Error>> {
    engine
        .append_history(session, vec![Message::system(AGENT_SYSTEM)])
        .await?;

    let turn = engine
        .turn(session)
        .input(Message::user(MISSION))
        .extension(
            "openai.chat:reasoning",
            serde_json::json!({ "enabled": true }),
        );
    let mut outcome = drive(turn.start()).await?;

    for _ in 0..MAX_APPROVALS {
        if outcome.session.phase != Phase::AwaitingApproval {
            break;
        }
        decide(engine, session).await?;
        outcome = drive(engine.resume(session)).await?;
    }
    Ok(())
}

/// Streams one turn to the console and returns its outcome.
///
/// # Errors
///
/// Propagates engine and turn failures.
async fn drive(mut stream: EngineStream) -> Result<TurnOutcome, Box<dyn Error>> {
    while let Some(event) = stream.next().await {
        render::event(&event?);
    }
    Ok(stream.finish().await?)
}

/// Prompts for every pending approval and records the operator's answer.
async fn decide(engine: &Engine, session: SessionId) -> Result<(), Box<dyn Error>> {
    for record in engine.pending_approvals(session).await? {
        let question = format!("Allow `{}` to run?", record.tool);
        if render::confirm(&question) {
            engine
                .approve(record.id, GrantScope::Call(record.call_id), "operator")
                .await?;
        } else {
            engine
                .deny(record.id, "operator", "the operator denied the call")
                .await?;
        }
    }
    Ok(())
}

/// Runs the two analyst models, one per half of the catalogue.
///
/// # Errors
///
/// Propagates engine construction failures.
async fn fan_out(key: &str, catalog: &Value) -> Result<(), Box<dyn Error>> {
    let providers = report(
        "providers analyst",
        render::CYAN,
        chat_engine(
            key,
            &env_or("PROVIDERS_MODEL", DEFAULT_PROVIDERS_MODEL),
            analyst_capabilities(),
        )?,
        PROVIDERS_SYSTEM,
        &catalog::providers_view(catalog),
    )
    .await;

    let models = report(
        "models analyst",
        render::MAGENTA,
        chat_engine(
            key,
            &env_or("MODELS_MODEL", DEFAULT_MODELS_MODEL),
            analyst_capabilities(),
        )?,
        MODELS_SYSTEM,
        &catalog::models_view(catalog),
    )
    .await;

    render::section("branch reports", render::BOLD);
    render::emit(&format!("{providers}\n\n{models}\n"));
    Ok(())
}

/// Runs one analyst. A provider failure (a rate limit on the free tier, say)
/// prints a line and yields an empty report instead of ending the run.
async fn report(title: &str, colour: &str, engine: Engine, system: &str, data: &Value) -> String {
    match branch::analyse(title, colour, &engine, system, data).await {
        Ok(text) => text.trim().to_owned(),
        Err(error) => render::failure(title, error.as_ref()),
    }
}

/// Builds the agent: the shell tool (approval-gated), the catalogue tool, and
/// the `OpenRouter` chat model.
fn agent_engine(
    key: &str,
    model: &str,
    url: &str,
    cache: Arc<Cache>,
) -> Result<Engine, Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_tool(&CommandTool::new())?;
    tools.register_declarator(&FetchCatalog::new(
        reqwest::Client::new(),
        url.to_owned(),
        cache,
    ))?;

    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "command-approval",
        vec![ToolName::new("command")?],
        "the agent wants to run a shell command",
    ))]);

    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(chat(key, model, agent_capabilities())?))
        .tools(tools)
        .policies(policies)
        .build()?)
}

/// Builds a tool-free engine for one analyst model.
fn chat_engine(
    key: &str,
    model: &str,
    capabilities: Capabilities,
) -> Result<Engine, Box<dyn Error>> {
    Ok(EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(chat(key, model, capabilities)?))
        .tools(ToolRegistry::new())
        .build()?)
}

/// Wires the `OpenAI` Chat adapter to `OpenRouter`.
fn chat(key: &str, model: &str, capabilities: Capabilities) -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let profile = VendorProfile::new(
        provider.clone(),
        WireFamily::new(WIRE_CHAT)?,
        OPENROUTER_URL,
    )?
    .with_auth(AuthScheme::bearer(key))
    .with_header("HTTP-Referer", "https://github.com/JeanVydes/rustychain")
    .with_header("X-Title", "RustyChain react-agent");

    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(model)?),
        profile,
        capabilities,
    )?)
}

fn agent_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

fn analyst_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build()
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}
