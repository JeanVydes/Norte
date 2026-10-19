//! Export and import: the transcript is the artifact, the store is where it
//! lives today.
//!
//! A session is exported as a self-contained `Conversation` — JSON a host can
//! keep, ship or hand to another process — and imported into a *fresh* engine.
//! The guarantee is not that the text survives (a transcript is just text) but
//! that the next request does: the same head reconstructs the same bytes, so a
//! provider cache stays warm and the model sees no seam.
//!
//! What is compared here are the wire bodies the engine actually asked the model
//! for — the same projection the live path POSTs (FR-RECON-12), captured by a
//! `Model` that records the request and forwards the call. The comparison is of
//! bytes, not of behaviour: two engines, one conversation, identical JSON.
//!
//! The last section is the pessimistic one. An import under *another* session id
//! is an explicit fork, and the difference is exactly the cache key the fork
//! declares (FR-EXP-2): the bytes are otherwise the same, so the miss is the
//! only thing that changed.
//!
//! ```sh
//! OPENROUTER_API_KEY=sk-or-... cargo run -p norte-cookbook --example export_import
//! ```

use std::error::Error;
use std::sync::{Arc, Mutex};

use norte_core::{
    Capabilities, ContinuityAdapter, Digest, Engine, EngineBuilder, InMemoryBlobStore,
    InMemoryStore, Message, Modality, Model, ModelId, ModelRef, ModelRequest, ModelStream,
    ProviderId, SessionId, StreamingGranularity, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT};
use serde_json::Value;

const SYSTEM: &str = "Answer in one short sentence. Do not use tools.";
const FIRST: &str = "Name the capital of Portugal.";
const NEXT: &str = "And of Spain?";

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // --- one engine has a conversation -------------------------------------
    let (engine, before) = new_engine()?;
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(SYSTEM)])
        .await?;
    let outcome = engine
        .send(session.id, Message::user(FIRST))
        .finish()
        .await?;
    println!("--- the conversation");
    println!("user  {FIRST}");
    println!("model {}", answer(outcome.message.as_ref()));

    // --- export: the transcript as data ------------------------------------
    let conversation = engine.export(session.id).await?;
    let envelope = conversation.to_json()?;
    println!("\n--- export");
    println!(
        "session {} · schema {} · phase {:?} · turn {} · {} message(s) · {} bytes of JSON",
        conversation.session,
        conversation.schema_version,
        conversation.phase,
        conversation.turn,
        conversation.messages.len(),
        envelope.len()
    );

    // --- import into a fresh engine ----------------------------------------
    let (forked, after) = new_engine()?;
    let restored = forked.import(conversation.clone()).await?;
    println!("\n--- import into a fresh engine");
    println!(
        "session {} ({}) · {} message(s) restored",
        restored.id,
        if restored.id == conversation.session {
            "the same id"
        } else {
            "a different id"
        },
        forked.history(restored.id).await?.len()
    );

    // --- the next request, byte for byte -----------------------------------
    engine
        .send(session.id, Message::user(NEXT))
        .finish()
        .await?;
    forked
        .send(restored.id, Message::user(NEXT))
        .finish()
        .await?;

    let original = last(&before);
    let imported = last(&after);
    let original_digest = Digest::of_json(&original);
    let imported_digest = Digest::of_json(&imported);
    println!("\n--- the next request, after the same `{NEXT}`");
    println!(
        "before export: {} bytes, digest {original_digest}",
        original.to_string().len()
    );
    println!(
        "after import:  {} bytes, digest {imported_digest}",
        imported.to_string().len()
    );
    println!(
        "identical: {}",
        if original == imported { "yes" } else { "NO" }
    );

    // --- the pessimistic case: a fork under another id ---------------------
    let mut fork = conversation;
    fork.session = SessionId::new();
    let (elsewhere, elsewhere_requests) = new_engine()?;
    let forked_session = elsewhere.import(fork).await?;
    elsewhere
        .send(forked_session.id, Message::user(NEXT))
        .finish()
        .await?;
    let diff = differences(&original, &last(&elsewhere_requests), "");
    println!(
        "\n--- the same envelope imported under another id ({})",
        forked_session.id
    );
    if diff.is_empty() {
        println!("no difference — which would mean the cache key is not the id");
    } else {
        for (key, from, to) in &diff {
            println!("{key}: {from} → {to}");
        }
    }
    Ok(())
}

/// The request the engine asked for last: every model call goes through the
/// probe, so the list is in call order and the tail is the one just compared.
fn last(captured: &Captured) -> Value {
    captured
        .lock()
        .expect("the probe lock is never held across an await")
        .last()
        .cloned()
        .expect("a request was captured")
}

/// Every leaf that differs between two projections, as `(path, first, second)`.
/// The two bodies are built by the same code from the same conversation, so a
/// difference is meaningful: it is a field something put there on purpose.
fn differences(a: &Value, b: &Value, path: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    walk(a, b, path, &mut out);
    out
}

fn walk(a: &Value, b: &Value, path: &str, out: &mut Vec<(String, String, String)>) {
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let at = format!("{path}.{key}");
                match b.get(key) {
                    Some(other) => walk(value, other, &at, out),
                    None => out.push((at, value.to_string(), "(absent)".to_owned())),
                }
            }
            for key in b.keys() {
                if !a.contains_key(key) {
                    out.push((
                        format!("{path}.{key}"),
                        "(absent)".to_owned(),
                        b[key].to_string(),
                    ));
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for index in 0..a.len().max(b.len()) {
                let at = format!("{path}[{index}]");
                match (a.get(index), b.get(index)) {
                    (Some(first), Some(second)) => walk(first, second, &at, out),
                    (Some(first), None) => out.push((at, first.to_string(), "(absent)".to_owned())),
                    (None, Some(second)) => {
                        out.push((at, "(absent)".to_owned(), second.to_string()));
                    }
                    (None, None) => {}
                }
            }
        }
        _ if a != b => out.push((path.to_owned(), a.to_string(), b.to_string())),
        _ => {}
    }
}

fn answer(message: Option<&Message>) -> String {
    message.map(Message::text).unwrap_or_default()
}

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

/// A fresh engine — its own store, its own probe — over the same model.
fn new_engine() -> Result<(Engine, Captured), Box<dyn Error>> {
    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .build();
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let model = Probe {
        inner: openrouter(capabilities)?,
        captured: Arc::clone(&captured),
    };
    let engine = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(model))
        .build()?;
    Ok((engine, captured))
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
