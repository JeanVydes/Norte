//! Rewind, branch, and what the provider's cache does with it.
//!
//! A conversation is a graph, and a session's head is just a pointer into it
//! (FR-GRAPH-1). Moving that pointer to an ancestor and asking a different
//! question is a **branch**: the ancestor is not rewritten, a second child is
//! added to it, and the new turn reconstructs the shared prefix exactly as it was
//! already sent (FR-RECON-8).
//!
//! This recipe does that with the store, which is the branching seam: there is no
//! separate rewind call. Then it prints the two numbers that matter:
//!
//! - the **bytes**: a digest of the message prefix the branch turn shares with
//!   the turn it forked from, and whether the two are byte-identical;
//! - the **tokens**: what the provider reported it read from its cache on the
//!   branch turn, because that is the only number that proves a hit.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example branching
//! ```
//!
//! `COOKBOOK_PROVIDER=Parasail` pins the upstream provider. `OpenRouter` serves
//! the same model id from several of them and a cache lives inside one, so an
//! unpinned run can report nothing even though the bytes are identical.
//!
//! The system message is deliberately long: a provider caches a prefix only past
//! a floor, and a short conversation cannot show a hit either way. Whether a hit
//! is reported is still the provider's decision, and with `OpenRouter` that means
//! the route in front of it: the same model id is served by several upstream
//! providers, they do not all implement prompt caching, and the one chosen can
//! change between runs. The recipe measures the route it actually got and says so
//! instead of pretending otherwise.

use std::error::Error;
use std::sync::{Arc, Mutex};

use norte_core::{
    Capabilities, Commit, ContinuityAdapter, Digest, Engine, EngineBuilder, InMemoryBlobStore,
    InMemoryStore, Message, Modality, Model, ModelId, ModelRef, ModelRequest, ModelStream, Node,
    NodeId, Phase, ProviderId, Session, Store, StreamingGranularity, ToolKind, ToolRegistry,
    TurnOptions, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use serde_json::{Value, json};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let (engine, store, captured) = new_engine()?;
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(manual())])
        .await?;

    let options = turn_options();
    println!(
        "--- route: {}",
        match std::env::var("COOKBOOK_PROVIDER") {
            Ok(provider) => format!("pinned to {provider}"),
            Err(_) => "OpenRouter decides, and may switch upstream between turns".to_owned(),
        }
    );

    println!("--- branch A: two turns");
    let first = engine
        .turn(session.id)
        .input(Message::user(ASK_A))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    report("turn 1", first.message.as_ref());
    let fork_point = engine
        .session(session.id)
        .await?
        .head
        .expect("the first turn committed a node");

    let second = engine
        .turn(session.id)
        .input(Message::user(ASK_B))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    report("turn 2", second.message.as_ref());
    let branch_a = last(&captured);

    println!("\n--- rewind to the first answer");
    println!("fork point: {fork_point}");
    let state = engine.session(session.id).await?;
    let branch_head = branch_from(&store, &state, fork_point, Message::user(ASK_C)).await?;
    println!(
        "the fork now has {} children; the head moved without rewriting a node",
        store.children(fork_point).await?.len()
    );

    let branch_turn = engine
        .turn(session.id)
        .options(options.clone())
        .start()
        .finish()
        .await?;
    let reused = report(
        "the branch turn (a new question at the fork)",
        branch_turn.message.as_ref(),
    );
    let branch_b = last(&captured);
    // Everything before the branch node is the prefix the provider may still
    // hold; the branch node itself is the new, divergent suffix.
    let fork_depth = store.ancestors(branch_head).await?.len() - 1;
    let prefix_stable = compare("shared with branch A", &branch_a, &branch_b, fork_depth);

    let next = engine
        .turn(session.id)
        .input(Message::user(ASK_D))
        .options(options.clone())
        .start()
        .finish()
        .await?;
    report("the branch's next turn", next.message.as_ref());
    let branch_b_next = last(&captured);
    let branch_depth = messages_of(&branch_b).len();
    compare(
        "shared with the branch's own turn",
        &branch_b,
        &branch_b_next,
        branch_depth,
    );

    println!("\n--- what this shows");
    if !prefix_stable {
        println!(
            "BUG: the branch did not reproduce its ancestor prefix byte-for-byte, so the provider \
             had to re-read it."
        );
    } else if reused.is_some_and(|read| read > 0) {
        println!(
            "the branch turn arrived with a byte-identical prefix and the provider reported \
             {} cached token(s) for it: the shared history was reused, not re-billed, and only \
             the divergent suffix is new.",
            reused.unwrap_or_default()
        );
    } else {
        println!(
            "the branch turn arrived with a byte-identical prefix, which is the library's half of \
             the bargain, but the provider reported no cache read. OpenRouter routes each model to \
             one of several upstream providers and they do not all cache, or report it: the same \
             model id can hit on one run and report nothing on the next. The bytes are the part \
             the library can prove; the hit is the route's call."
        );
    }
    Ok(())
}

/// Moves the session head to a new child of `parent`: that is the branch. The
/// ancestor is never rewritten, and the fork gains a second child.
async fn branch_from(
    store: &InMemoryStore,
    session: &Session,
    parent: NodeId,
    message: Message,
) -> Result<NodeId, Box<dyn Error>> {
    let node = Node::message(message, Some(parent));
    let head = node.id;
    store
        .commit(Commit {
            session_id: session.id,
            expected_version: Some(session.version),
            nodes: vec![node],
            head: Some(head),
            phase: Phase::Idle,
            turn: session.turn,
            grants: session.grants.clone(),
            metadata: None,
        })
        .await?;
    Ok(head)
}

/// The usage of one turn, and the cache read it reported.
fn report(label: &str, message: Option<&Message>) -> Option<u64> {
    let usage = message
        .and_then(|message| message.origin.as_ref())
        .map(|origin| &origin.usage);
    let Some(usage) = usage else {
        println!("{label}: no usage reported");
        return None;
    };
    println!(
        "{label}: input {} · output {} · cache read {}",
        show(usage.input_tokens),
        show(usage.output_tokens),
        show(usage.cache_read_tokens)
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

/// Compares the `shared` messages a request was expected to reproduce with the
/// request they came from, and prints their digest. The count comes from the
/// graph — how many messages sit before the fork — not from guessing where the
/// bytes stop matching: a branch is *supposed* to diverge at its suffix.
fn compare(label: &str, before: &Value, after: &Value, shared: usize) -> bool {
    let before = messages_of(before);
    let after = messages_of(after);
    let identical =
        before.len() >= shared && after.len() >= shared && before[..shared] == after[..shared];
    let digest = Digest::of_json(&Value::Array(after[..shared].to_vec()));
    println!(
        "{label}: the first {shared} of {} message(s) — {} ({digest})",
        after.len(),
        if identical {
            "byte-identical"
        } else {
            "CHANGED"
        }
    );
    identical
}

fn messages_of(request: &Value) -> Vec<Value> {
    request
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The last wire body the probe recorded.
fn last(captured: &Captured) -> Value {
    captured
        .lock()
        .expect("the probe lock is never held across an await")
        .last()
        .cloned()
        .expect("every turn records its request before calling the model")
}

/// A long, deterministic system message: the prefix has to be past the
/// provider's caching floor before any of this is observable.
fn manual() -> String {
    use std::fmt::Write as _;

    let mut out = String::from(
        "You are a concise assistant. Answer in one short sentence. The numbered rules below \
         are standing instructions; each one is a rule.\n",
    );
    for rule in 1..=140 {
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

const ASK_A: &str = "In one sentence: what is a prompt cache?";
const ASK_B: &str = "And in one sentence: what is a cache miss?";
const ASK_C: &str = "In one sentence: why does a cache need a stable prefix?";
const ASK_D: &str = "And in one sentence: what breaks that prefix?";

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

fn new_engine() -> Result<(Engine, Arc<InMemoryStore>, Captured), Box<dyn Error>> {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let model = Probe {
        inner: openrouter()?,
        captured: Arc::clone(&captured),
    };
    let store = Arc::new(InMemoryStore::new());
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .tools(ToolRegistry::new())
        .build()?;
    Ok((engine, store, captured))
}

fn capabilities() -> Capabilities {
    Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .build()
}

/// Pins the upstream provider when `COOKBOOK_PROVIDER` names one.
///
/// `OpenRouter` load-balances a model across several providers, and a prompt
/// cache lives inside one of them: a run that switches upstream between turns
/// reports no hit even though the bytes never changed. Pinning makes the
/// measurement mean something, and a vendor request extra is how a host asks for
/// routing it cares about (FR-ENG-8).
fn turn_options() -> TurnOptions {
    let mut options = TurnOptions::default();
    if let Ok(provider) = std::env::var("COOKBOOK_PROVIDER") {
        options.extensions.insert(
            "openai.chat:provider".to_owned(),
            json!({ "only": [provider], "allow_fallbacks": false }),
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
fn openrouter() -> Result<OpenAiChat, Box<dyn Error>> {
    let provider = ProviderId::new("openrouter")?;
    let endpoint = required("COOKBOOK_ENDPOINT", "https://openrouter.ai/api/v1")?;
    let profile = VendorProfile::new(provider.clone(), WireFamily::new(WIRE_CHAT)?, &endpoint)?
        .with_auth(AuthScheme::bearer(std::env::var("OPENROUTER_API_KEY")?));
    let id = required("COOKBOOK_MODEL", "deepseek/deepseek-v4.1-flash-1017")?;
    Ok(OpenAiChat::new(
        ModelRef::new(provider, ModelId::new(&id)?),
        profile,
        capabilities(),
    )?)
}
