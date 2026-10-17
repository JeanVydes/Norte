//! Export/import byte-identity (FR-EXP-1, FR-EXP-2, FR-EXP-6, FR-RECON-12).
//!
//! The guarantee under test: a conversation exported and re-imported produces a
//! reconstructed request — and therefore a provider body — that is byte-for-byte
//! identical to the original, including the session-derived cache key. A cache
//! miss here is money, so these are the tests that must never regress.

use std::sync::Arc;

use rustychain_core::{
    CallId, Conversation, Engine, EngineBuilder, FnExecutor, Message, Model, ModelRef,
    ModelRequest, Part, Result, Role, TextPart, ToolCallPart, ToolRegistry, ToolResultPart,
};
use rustychain_openai::OpenAiChat;
use rustychain_testing::{InMemoryBlobStore, InMemoryStore, ProbeModel, text_capabilities};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WeatherArgs {
    city: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Weather {
    conditions: String,
}

#[rustychain_core::tool(name = "weather", description = "Look up the weather")]
struct WeatherTool;

#[async_trait::async_trait]
impl FnExecutor<WeatherArgs, Weather> for WeatherTool {
    async fn call(&self, args: WeatherArgs) -> Result<Weather> {
        Ok(Weather {
            conditions: args.city,
        })
    }
}

fn registry() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools
        .register_declarator(&WeatherTool)
        .expect("register weather");
    tools
}

fn engine(model: Arc<dyn Model>, store: Arc<InMemoryStore>) -> Engine {
    EngineBuilder::new()
        .store(store)
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(model)
        .tools(registry())
        .build()
        .expect("engine")
}

/// A conversation with a system prompt, a tool round and a follow-up so the
/// request mapping exercises instructions, tool calls, tool results and roles.
fn seed() -> Vec<Message> {
    let call_id = CallId::new();
    vec![
        Message::system("You are terse."),
        Message::user("What is the weather in Paris?"),
        Message::new(
            Role::Assistant,
            vec![
                Part::Text(TextPart::new("Let me check.")),
                Part::ToolCall(ToolCallPart {
                    call_id,
                    name: "weather".to_owned(),
                    arguments: json!({ "city": "Paris" }),
                    continuity: None,
                }),
            ],
        ),
        Message::tool(vec![ToolResultPart::text(call_id, "sunny")]),
        Message::user("And tomorrow?"),
    ]
}

async fn assert_wire_stable(model: Arc<dyn Model>, body_of: impl Fn(&ModelRequest) -> Value) {
    let seed = seed();

    // Original engine: seed the history, then export it.
    let store_a = Arc::new(InMemoryStore::new());
    let probe_a = Arc::new(ProbeModel::new(model.clone()));
    let engine_a = engine(probe_a.clone(), store_a);
    let session = engine_a.create().await.expect("create");
    engine_a
        .append_history(session.id, seed.clone())
        .await
        .expect("seed");
    let conversation = engine_a.export(session.id).await.expect("export");

    // The envelope round-trips byte-identically.
    let json = conversation.to_json().expect("to_json");
    let repacked = Conversation::from_json(&json)
        .expect("from_json")
        .to_json()
        .expect("to_json");
    assert_eq!(json, repacked, "conversation JSON must be byte-stable");

    // Restored engine: import the exact envelope.
    let store_b = Arc::new(InMemoryStore::new());
    let probe_b = Arc::new(ProbeModel::new(model.clone()));
    let engine_b = engine(probe_b.clone(), store_b);
    let imported = engine_b
        .import(Conversation::from_json(&json).expect("from_json"))
        .await
        .expect("import");
    assert_eq!(
        imported.id, session.id,
        "import must preserve the session id, or the provider cache key moves"
    );
    assert_eq!(
        engine_b.history(imported.id).await.expect("history"),
        seed,
        "history must round-trip losslessly"
    );

    // Both engines reconstruct the same next turn.
    engine_a
        .send(session.id, Message::user("and now?"))
        .finish()
        .await
        .expect("send a");
    engine_b
        .send(imported.id, Message::user("and now?"))
        .finish()
        .await
        .expect("send b");

    let request_a = probe_a.last_request().expect("request a");
    let request_b = probe_b.last_request().expect("request b");
    assert_eq!(
        serde_json::to_string(&request_a).expect("request a bytes"),
        serde_json::to_string(&request_b).expect("request b bytes"),
        "the reconstructed request must be byte-identical after export/import"
    );

    let body_a = body_of(&request_a);
    let body_b = body_of(&request_b);
    assert_eq!(body_a, body_b, "the provider body must be identical");
    assert_eq!(
        serde_json::to_string(&body_a).expect("bytes a"),
        serde_json::to_string(&body_b).expect("bytes b"),
        "the provider bytes must be identical"
    );
}

#[tokio::test]
async fn openai_responses_wire_is_byte_identical_after_export_import() {
    let adapter = rustychain_openai::presets::openai("gpt-5", "sk-test", text_capabilities())
        .expect("openai");
    let body_of = {
        let adapter = adapter.clone();
        move |request: &ModelRequest| adapter.request_body(request).expect("body")
    };
    assert_wire_stable(Arc::new(adapter), body_of).await;
}

#[tokio::test]
async fn openai_chat_wire_is_byte_identical_after_export_import() {
    let profile = rustychain_openai::presets::openai_profile("sk-test").expect("profile");
    let reference = ModelRef::new(profile.provider.clone(), "gpt-5".parse().expect("model id"));
    let adapter = OpenAiChat::new(reference, profile, text_capabilities()).expect("chat");
    let body_of = {
        let adapter = adapter.clone();
        move |request: &ModelRequest| adapter.request_body(request).expect("body")
    };
    assert_wire_stable(Arc::new(adapter), body_of).await;
}

#[tokio::test]
async fn anthropic_wire_is_byte_identical_after_export_import() {
    let adapter =
        rustychain_anthropic::presets::anthropic("claude-sonnet-4", "sk-test", text_capabilities())
            .expect("anthropic");
    let body_of = {
        let adapter = adapter.clone();
        move |request: &ModelRequest| adapter.request_body(request).expect("body")
    };
    assert_wire_stable(Arc::new(adapter), body_of).await;
}

#[tokio::test]
async fn gemini_wire_is_byte_identical_after_export_import() {
    let adapter =
        rustychain_gemini::presets::gemini("gemini-2.5-pro", "sk-test", text_capabilities())
            .expect("gemini");
    let body_of = {
        let adapter = adapter.clone();
        move |request: &ModelRequest| adapter.request_body(request).expect("body")
    };
    assert_wire_stable(Arc::new(adapter), body_of).await;
}

#[tokio::test]
async fn importing_a_duplicate_session_is_rejected() {
    let adapter = rustychain_openai::presets::openai("gpt-5", "sk-test", text_capabilities())
        .expect("openai");
    let store = Arc::new(InMemoryStore::new());
    let engine = engine(Arc::new(ProbeModel::new(Arc::new(adapter))), store);

    let session = engine.create().await.expect("create");
    engine
        .append_history(session.id, seed())
        .await
        .expect("seed");
    let conversation = engine.export(session.id).await.expect("export");

    let error = engine
        .import(conversation)
        .await
        .expect_err("importing an existing id must fail");
    assert!(matches!(
        error,
        rustychain_core::Error::StoreConflict { .. }
    ));
}

#[tokio::test]
async fn a_forked_conversation_moves_only_the_cache_key() {
    // Importing under a fresh id is an explicit fork: the prefix stays
    // byte-identical, but the session-derived cache key moves, which is exactly
    // why the envelope preserves the original id by default.
    let adapter = rustychain_openai::presets::openai("gpt-5", "sk-test", text_capabilities())
        .expect("openai");
    let body_of = {
        let adapter = adapter.clone();
        move |request: &ModelRequest| adapter.request_body(request).expect("body")
    };

    let store = Arc::new(InMemoryStore::new());
    let probe = Arc::new(ProbeModel::new(Arc::new(adapter)));
    let engine = engine(probe.clone(), store);
    let session = engine.create().await.expect("create");
    engine
        .append_history(session.id, seed())
        .await
        .expect("seed");

    let mut conversation = engine.export(session.id).await.expect("export");
    conversation.session = rustychain_core::SessionId::new();
    let forked = engine.import(conversation).await.expect("import fork");
    assert_ne!(forked.id, session.id);

    engine
        .send(session.id, Message::user("and now?"))
        .finish()
        .await
        .expect("send original");
    engine
        .send(forked.id, Message::user("and now?"))
        .finish()
        .await
        .expect("send fork");

    let original = body_of(&probe.requests()[0]);
    let forked_body = body_of(&probe.requests()[1]);
    assert_eq!(
        original.get("prompt_cache_key"),
        Some(&Value::String(session.id.to_string()))
    );
    assert_eq!(
        forked_body.get("prompt_cache_key"),
        Some(&Value::String(forked.id.to_string()))
    );

    // Everything except the cache key is identical.
    let mut without_key = original.clone();
    without_key
        .as_object_mut()
        .expect("object")
        .remove("prompt_cache_key");
    let mut forked_without_key = forked_body.clone();
    forked_without_key
        .as_object_mut()
        .expect("object")
        .remove("prompt_cache_key");
    assert_eq!(without_key, forked_without_key);
}
