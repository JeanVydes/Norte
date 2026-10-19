//! The provider cache, measured: what survives a turn, a tool call, a park and
//! an export.
//!
//! Two different things are being watched, and they belong to different owners.
//! The **bytes** are the library's: reconstruction is append-only and
//! deterministic (FR-RECON-1, FR-RECON-2), so every request starts with exactly
//! the message prefix of the one before it, and a request that grows only at the
//! end is a request whose prefix a provider may still hold. The **tokens** are
//! the provider's: it reports what it actually read from its cache in
//! `Usage::cache_read_tokens`, and that is the only number that says a cache hit
//! really happened.
//!
//! So every phase prints both: the usage the provider reported, and a digest of
//! the message prefix shared with the previous request — the attribution. A
//! cache read that is not explained by an identical prefix would be a bug in the
//! library; an identical prefix with no cache read is the provider deciding
//! (a short prefix, an expired entry, a model that does not cache at all).
//!
//! The four phases are the four things a host worries about:
//!
//! 1. the normal case — the next turn re-sends the conversation;
//! 2. after a tool call — the tool exchange joined the prefix;
//! 3. after human approval — the turn parked, and the resume re-sends what the
//!    model already answered;
//! 4. after an export and an import into a fresh engine — the transcript is new
//!    memory, the bytes must not be.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example cache
//! ```
//!
//! The prefix has to be worth caching before any of this is visible: providers
//! only cache a prefix past a floor (often about a thousand tokens), so the
//! system message below is a long one.
//!
//! And whether a hit is *reported* is the provider's: the model here is whatever
//! `COOKBOOK_MODEL` names, and a free model's provider reports cache usage only
//! when it keeps a cache and says so. `COOKBOOK_MODEL=openai/gpt-6-luna` runs
//! the identical recipe against a provider that reports it, so the two halves —
//! the bytes, which the library controls, and the tokens, which it does not —
//! can be watched next to each other.
//!
//! `COOKBOOK_PROVIDER=OpenInference` pins the upstream provider. Without it
//! `OpenRouter` may route two turns of the same conversation to different
//! providers, and each provider keeps its own cache: the hit is then a routing
//! decision, not a property of the prefix.

use std::error::Error;
use std::sync::{Arc, Mutex};

use norte_core::{
    Capabilities, ContinuityAdapter, Digest, Engine, EngineBuilder, FnExecutor, GrantScope,
    InMemoryBlobStore, InMemoryStore, Message, Modality, Model, ModelId, ModelRef, ModelRequest,
    ModelStream, PolicySet, ProviderId, SessionId, StreamingGranularity, ToolApprovalPolicy,
    ToolKind, ToolName, ToolRegistry, TurnOptions, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (engine, captured) = new_engine()?;
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(manual())])
        .await?;
    let mut hits = Hits::default();
    let options = turn_options();

    println!(
        "--- route: {}",
        match std::env::var("COOKBOOK_PROVIDER") {
            Ok(provider) => format!("pinned to {provider}"),
            Err(_) => "OpenRouter decides, and may switch upstream between turns".to_owned(),
        }
    );

    println!("--- 1. the normal case");
    normal(&engine, session.id, &captured, &mut hits, &options).await?;

    println!("\n--- 2. after a tool call");
    after_tool(&engine, session.id, &captured, &mut hits, &options).await?;

    println!("\n--- 3. after human approval");
    after_approval(&engine, session.id, &captured, &mut hits, &options).await?;

    println!("\n--- 4. after an export and an import into a fresh engine");
    after_export(&engine, &captured, session.id, &mut hits, &options).await?;

    closing(&hits);
    Ok(())
}

/// The normal case: the next turn re-sends the conversation, and that is all.
async fn normal(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    hits: &mut Hits,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let outcome = engine
        .turn(session)
        .input(Message::user(ASK_A))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "turn 1 (nothing to reuse yet)",
        outcome.message.as_ref(),
    ));
    let outcome = engine
        .turn(session)
        .input(Message::user(ASK_B))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "turn 2 (the conversation again)",
        outcome.message.as_ref(),
    ));
    audit(captured, None);
    Ok(())
}

/// After a tool call: the call and its result join the prefix, at the end.
async fn after_tool(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    hits: &mut Hits,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let outcome = engine
        .turn(session)
        .input(Message::user(COUNT_WORDS))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "the turn that calls the tool",
        outcome.message.as_ref(),
    ));
    let outcome = engine
        .turn(session)
        .input(Message::user("How many was that?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "the turn after the result",
        outcome.message.as_ref(),
    ));
    audit(captured, None);
    Ok(())
}

/// After approval: the parked turn is not rebuilt when it resumes — the resume
/// re-sends the same prefix the park already sent.
async fn after_approval(
    engine: &Engine,
    session: SessionId,
    captured: &Captured,
    hits: &mut Hits,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let outcome = engine
        .turn(session)
        .input(Message::user(DEPLOY))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    println!("phase: {:?}", outcome.session.phase);
    for record in engine.pending_approvals(session).await? {
        println!("waiting on {} (approval {})", record.tool, record.id);
        engine
            .approve(record.id, GrantScope::Call(record.call_id), "operator")
            .await?;
    }
    let outcome = engine.resume(session).finish().await?;
    hits.add(report(
        "the resume (not a new request: the parked one)",
        outcome.message.as_ref(),
    ));
    let outcome = engine
        .turn(session)
        .input(Message::user("What did the deploy return?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "the turn after the approval",
        outcome.message.as_ref(),
    ));
    audit(captured, None);
    Ok(())
}

/// After an export: the conversation is imported into a fresh engine and asked
/// to carry on — the transcript is new memory, the bytes must not be.
async fn after_export(
    engine: &Engine,
    captured: &Captured,
    session: SessionId,
    hits: &mut Hits,
    options: &TurnOptions,
) -> Result<(), Box<dyn Error>> {
    let conversation = engine.export(session).await?;
    println!(
        "envelope: session {} · {} message(s) · {} bytes of JSON",
        conversation.session,
        conversation.messages.len(),
        conversation.to_json()?.len()
    );
    let (elsewhere, requests) = new_engine()?;
    let restored = elsewhere.import(conversation).await?;
    println!(
        "restored session: {} ({})",
        restored.id,
        if restored.id == session {
            "the same id"
        } else {
            "A DIFFERENT ID"
        }
    );
    let outcome = elsewhere
        .turn(restored.id)
        .input(Message::user("In one line: what have we done so far?"))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    hits.add(report(
        "the first turn of the new process",
        outcome.message.as_ref(),
    ));
    audit(&requests, Some(captured));
    Ok(())
}

/// The closing word, whichever way the provider behaved.
fn closing(hits: &Hits) {
    println!("\n--- what this shows");
    if hits.turns == 0 {
        println!(
            "the provider reported no cache read on any turn, so no live hit is proven here. \
             The prefixes above are still byte-identical, which is what the library promises and \
             the provider's side of the bargain; a provider that does not report cache usage \
             (or a prefix under its floor) cannot show one."
        );
    } else {
        println!(
            "the provider reported {} cached token(s) across {} turn(s), each one on a turn whose \
             shared prefix the audit above shows byte-identical to the request before it — \
             including the resume after the approval and the first turn after the import.",
            hits.tokens, hits.turns
        );
    }
}

/// The turns that read anything from the provider's cache.
#[derive(Default)]
struct Hits {
    turns: usize,
    tokens: u64,
}

impl Hits {
    fn add(&mut self, read: Option<u64>) {
        if let Some(read) = read.filter(|read| *read > 0) {
            self.turns += 1;
            self.tokens += read;
        }
    }
}

/// The usage of one turn, as the provider reported it. Returns the cache read,
/// so the caller can count the turns that actually hit.
fn report(label: &str, message: Option<&Message>) -> Option<u64> {
    let usage = message
        .and_then(|message| message.origin.as_ref())
        .map(|origin| &origin.usage);
    let Some(usage) = usage else {
        println!("{label}: no usage reported");
        return None;
    };
    println!(
        "{label}: input {} · output {} · cache read {} · cache write {}",
        show(usage.input_tokens),
        show(usage.output_tokens),
        show(usage.cache_read_tokens),
        show(usage.cache_write_tokens)
    );
    if let (Some(input), Some(read)) = (usage.input_tokens, usage.cache_read_tokens) {
        if input > 0 {
            let percent = f64::from(u32::try_from(read).unwrap_or(u32::MAX)) * 100.0
                / f64::from(u32::try_from(input).unwrap_or(1));
            println!("  the provider read {percent:.1}% of the input from its cache");
        }
    }
    usage.cache_read_tokens
}

fn show(value: Option<u64>) -> String {
    value.map_or_else(|| "—".to_owned(), |value| value.to_string())
}

/// The bytes, request by request: the message prefix shared with the request
/// before it, and its digest. This is the half of a cache hit the library owns.
///
/// `before` seeds the comparison with another engine's calls, so the first
/// request of a process that imported a conversation is compared against the
/// last request of the process that exported it — which is the whole point of
/// the last phase.
fn audit(captured: &Captured, before: Option<&Captured>) {
    let mut lines: Vec<(String, Value)> = Vec::new();
    if let Some(before) = before {
        let requests = before
            .lock()
            .expect("the probe lock is never held across an await")
            .clone();
        if let Some(last) = requests.last() {
            lines.push(("the other engine's last request".to_owned(), last.clone()));
        }
    }
    for (index, request) in captured
        .lock()
        .expect("the probe lock is never held across an await")
        .iter()
        .enumerate()
    {
        lines.push((format!("request {}", index + 1), request.clone()));
    }

    let mut previous: Option<(String, Vec<Value>)> = None;
    for (label, request) in &lines {
        let messages = messages_of(request);
        let bytes = request.to_string().len();
        match &previous {
            None => println!("  {label}: {} message(s), {bytes} bytes", messages.len()),
            Some((before, previous)) => {
                let shared = messages.len().min(previous.len());
                let same = previous.len() >= shared && messages[..shared] == previous[..shared];
                let digest = Digest::of_json(&Value::Array(messages[..shared].to_vec()));
                println!(
                    "  {label}: {} message(s), {bytes} bytes, {shared} shared with {before} — {} ({digest})",
                    messages.len(),
                    if same { "byte-identical" } else { "CHANGED" }
                );
            }
        }
        previous = Some((label.clone(), messages));
    }
}

fn messages_of(request: &Value) -> Vec<Value> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// A long, deterministic system message: the prefix has to be past the
/// provider's caching floor before any of this is observable.
fn manual() -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "You are a concise assistant. Answer in one short sentence. \
         If asked to count words, you MUST call the `word_count` tool with the text. \
         If asked to deploy, you MUST call the `deploy` tool. \
         The numbered rules below are standing instructions; each one is a rule.\n",
    );
    for rule in 1..=180 {
        writeln!(
            out,
            "Rule {rule}: instruction number {rule} stands on its own and applies to every answer; \
             it is stated here so that the conversation has a prefix long enough to be worth \
             caching, and rule {rule} is not special in any other way."
        )
        .expect("writing to a String never fails");
    }
    out
}

// ── the tools ───────────────────────────────────────────────────────────────

#[norte_core::tool(name = "word_count", description = "Count the words in a text")]
struct WordCount;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WordCountArgs {
    text: String,
}

#[derive(Debug, Serialize)]
struct WordCountResult {
    words: usize,
}

#[async_trait::async_trait]
impl FnExecutor<WordCountArgs, WordCountResult> for WordCount {
    async fn call(&self, args: WordCountArgs) -> norte_core::Result<WordCountResult> {
        Ok(WordCountResult {
            words: args.text.split_whitespace().count(),
        })
    }
}

#[norte_core::tool(name = "deploy", description = "Deploy a service to an environment")]
struct Deploy;

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeployArgs {
    service: String,
    environment: String,
}

#[derive(Debug, Serialize)]
struct DeployResult {
    url: String,
}

#[async_trait::async_trait]
impl FnExecutor<DeployArgs, DeployResult> for Deploy {
    async fn call(&self, args: DeployArgs) -> norte_core::Result<DeployResult> {
        Ok(DeployResult {
            url: format!("https://{}.example.test/{}", args.environment, args.service),
        })
    }
}

const ASK_A: &str = "In one sentence: what is a prompt cache?";
const ASK_B: &str = "And in one sentence: what is a cache miss?";
const COUNT_WORDS: &str = "Count the words in this exact text: one two three four.";
const DEPLOY: &str = "Deploy the service checkout to staging.";

// ── the wiring ──────────────────────────────────────────────────────────────

/// The wire bodies a model was asked for, in call order.
type Captured = Arc<Mutex<Vec<Value>>>;

/// A model that records the exact body of every request and then makes the call
/// anyway, so the recipe stays live: what is compared is what was sent.
struct Probe {
    inner: OpenAiChat,
    captured: Captured,
}

#[async_trait::async_trait]
impl Model for Probe {
    fn reference(&self) -> &ModelRef {
        self.inner.reference()
    }

    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn continuity(&self) -> &dyn ContinuityAdapter {
        self.inner.continuity()
    }

    async fn stream(&self, request: ModelRequest) -> norte_core::Result<ModelStream> {
        // `request_body` is the projection the live path uses (FR-RECON-12), so
        // recording it records the bytes, not an approximation of them.
        let body = self.inner.request_body(&request)?;
        self.captured
            .lock()
            .expect("the probe lock is never held across an await")
            .push(body);
        self.inner.stream(request).await
    }
}

/// A fresh engine — its own store, its own probe — over the same model, with
/// both tools declared from the start: changing the exposed set between turns
/// would change the request for reasons that have nothing to do with caching.
fn new_engine() -> Result<(Engine, Captured), Box<dyn Error>> {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&WordCount)?;
    tools.register_declarator(&Deploy)?;
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "human",
        vec![ToolName::new("deploy")?],
        "deploying touches production",
    ))]);
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let model = Probe {
        inner: openrouter(tool_capabilities())?,
        captured: Arc::clone(&captured),
    };
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(tools)
        .policies(policies)
        .build()?;
    Ok((engine, captured))
}

fn tool_capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

/// Pins the upstream provider when `COOKBOOK_PROVIDER` names one.
///
/// The measurement is about a prefix, and the cache that holds it lives inside
/// one provider: with `OpenRouter` free to route, a turn can land elsewhere and
/// report a miss the bytes never caused. Pinning turns the number back into a
/// statement about the provider the run names.
fn turn_options() -> TurnOptions {
    let mut options = TurnOptions::default();
    // Every answer here is a line or two. Saying so also keeps `OpenRouter` from
    // reserving the model's whole output window against the key on every call.
    options.params.max_output_tokens = Some(512);
    if let Ok(provider) = std::env::var("COOKBOOK_PROVIDER") {
        options.extensions.insert(
            "openai.chat:provider".to_owned(),
            serde_json::json!({ "order": [provider], "allow_fallbacks": false }),
        );
    }
    options
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
