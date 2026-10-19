//! Export/import byte-identity (FR-EXP-1, FR-EXP-2, FR-EXP-6, FR-RECON-12).
//!
//! The guarantee under test: a conversation exported and re-imported produces a
//! reconstructed request — and therefore a provider body — that is byte-for-byte
//! identical to the original, including the session-derived cache key. A cache
//! miss here is money, so these are the tests that must never regress.

use std::sync::Arc;

use rustychain_core::{
    CallId, ContinuityScope, ContinuityToken, Conversation, Engine, EngineBuilder, Error,
    FnExecutor, GrantScope, Message, Model, ModelRef, ModelRequest, Part, Phase, PolicySet,
    ReasoningPart, ReasoningVisibility, Result, Role, TextPart, ToolApprovalPolicy, ToolCallPart,
    ToolName, ToolRegistry, ToolResultPart, WireFamily,
};
use rustychain_openai::OpenAiChat;
use rustychain_testing::{
    InMemoryBlobStore, InMemoryStore, ProbeModel, ScriptedModel, StrictContinuity, model,
    text_capabilities,
};
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
async fn a_parked_approval_survives_export_import() {
    // The ledger does not travel in the envelope (FR-EXP-1), but the parked
    // state does. Importing into a fresh engine and resuming must ask for the
    // approval again instead of stalling on a record that no longer exists.
    let call_id = CallId::new();
    let policies = PolicySet::new(vec![Arc::new(ToolApprovalPolicy::new(
        "guard",
        vec![ToolName::new("weather").expect("tool")],
        "weather touches the world",
    ))]);

    let scripted = Arc::new(
        ScriptedModel::new(rustychain_testing::model("test/weather"))
            .turn_tool_call(call_id, "weather", &json!({ "city": "Paris" }))
            .turn_text("done"),
    );
    let engine_a = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(registry())
        .policies(policies.clone())
        .build()
        .expect("engine a");

    let session = engine_a.create().await.expect("create");
    let outcome = engine_a
        .send(session.id, Message::user("weather in Paris?"))
        .finish()
        .await
        .expect("turn");
    assert_eq!(outcome.session.phase, Phase::AwaitingApproval);

    let conversation = engine_a.export(session.id).await.expect("export");
    assert_eq!(conversation.phase, Phase::AwaitingApproval);

    let engine_b = EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(Arc::new(
            ScriptedModel::new(rustychain_testing::model("test/weather")).turn_text("done"),
        ))
        .tools(registry())
        .policies(policies)
        .build()
        .expect("engine b");
    let imported = engine_b.import(conversation).await.expect("import");
    assert!(
        engine_b
            .pending_approvals(imported.id)
            .await
            .expect("pending")
            .is_empty(),
        "no approval record travels in the envelope"
    );

    let parked = engine_b
        .resume(imported.id)
        .finish()
        .await
        .expect("resume re-requests");
    assert_eq!(parked.session.phase, Phase::AwaitingApproval);

    let fresh = engine_b
        .pending_approvals(imported.id)
        .await
        .expect("pending");
    assert_eq!(fresh.len(), 1, "resuming must re-request the approval");
    assert_eq!(fresh[0].call_id, call_id);

    engine_b
        .approve(fresh[0].id, GrantScope::Call(call_id), "tester")
        .await
        .expect("approve");
    let outcome = engine_b
        .resume(imported.id)
        .finish()
        .await
        .expect("resume after approval");
    assert_eq!(outcome.session.phase, Phase::Idle);
    assert_eq!(
        engine_b.history(imported.id).await.expect("history").len(),
        4,
        "user, assistant tool call, tool result, assistant answer"
    );
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

/// A probe over a scripted model whose continuity table is the strict one: it
/// replays only for its own exact model and refuses a cross-model replay while a
/// tool result is still owed (FR-CONT-2, FR-CONT-4).
fn strict(reference: ModelRef) -> Arc<ProbeModel> {
    let scripted = ScriptedModel::new(reference.clone())
        .with_continuity(Arc::new(StrictContinuity::new(reference)));
    Arc::new(ProbeModel::new(Arc::new(scripted)))
}

fn signature(reference: &ModelRef) -> ContinuityToken {
    ContinuityToken::new(
        WireFamily::new("test.strict").expect("family"),
        ContinuityScope::Exact(reference.clone()),
        1,
        b"opaque thinking signature".to_vec(),
    )
}

/// A history whose assistant turn carries a vendor signature, optionally ending
/// mid tool loop: the state in which dropping the signature makes a vendor
/// reject the next request.
fn signed(reference: &ModelRef, mid_tool_loop: bool) -> Vec<Message> {
    let call_id = CallId::new();
    let mut history = vec![
        Message::system("You are terse."),
        Message::user("Why is the sky blue?"),
        Message::new(
            Role::Assistant,
            vec![Part::Reasoning(ReasoningPart {
                visibility: ReasoningVisibility::Summary,
                text: "Rayleigh scattering.".to_owned(),
                continuity: Some(signature(reference)),
            })],
        ),
    ];
    if mid_tool_loop {
        history.push(Message::new(
            Role::Assistant,
            vec![Part::ToolCall(ToolCallPart {
                call_id,
                name: "weather".to_owned(),
                arguments: json!({ "city": "Paris" }),
                continuity: None,
            })],
        ));
        history.push(Message::tool(vec![ToolResultPart::text(call_id, "sunny")]));
    }
    history
}

async fn exported(reference: &ModelRef, mid_tool_loop: bool) -> Conversation {
    let source = strict(reference.clone());
    let engine = engine(source, Arc::new(InMemoryStore::new()));
    let session = engine.create().await.expect("create");
    engine
        .append_history(session.id, signed(reference, mid_tool_loop))
        .await
        .expect("seed");
    engine.export(session.id).await.expect("export")
}

fn carried_tokens(request: &ModelRequest) -> usize {
    request
        .messages
        .iter()
        .flat_map(|message| &message.parts)
        .filter_map(Part::continuity)
        .count()
}

#[tokio::test]
async fn export_import_carries_signatures_that_the_same_model_replays() {
    let reference = model("test/strict");
    let source = strict(reference.clone());
    let engine_a = engine(source.clone(), Arc::new(InMemoryStore::new()));
    let session = engine_a.create().await.expect("create");
    engine_a
        .append_history(session.id, signed(&reference, false))
        .await
        .expect("seed");
    let conversation = engine_a.export(session.id).await.expect("export");

    // The envelope is the transcript: the signature travels with it. Stripping
    // it would make a restored session send a different prefix (FR-EXP-2).
    assert_eq!(
        conversation
            .messages
            .iter()
            .flat_map(|message| &message.parts)
            .filter_map(Part::continuity)
            .count(),
        1,
        "the export carries the signature"
    );

    let target = strict(reference.clone());
    let engine_b = engine(target.clone(), Arc::new(InMemoryStore::new()));
    let imported = engine_b
        .import(Conversation::from_json(&conversation.to_json().expect("json")).expect("decode"))
        .await
        .expect("import");

    engine_a
        .send(session.id, Message::user("and at sunset?"))
        .finish()
        .await
        .expect("send a");
    engine_b
        .send(imported.id, Message::user("and at sunset?"))
        .finish()
        .await
        .expect("send b");

    let request_a = source.last_request().expect("request a");
    let request_b = target.last_request().expect("request b");
    assert_eq!(carried_tokens(&request_b), 1, "the model replays it");
    assert_eq!(
        serde_json::to_string(&request_a).expect("bytes a"),
        serde_json::to_string(&request_b).expect("bytes b"),
        "an identically configured engine reproduces the request byte-identically"
    );
}

#[tokio::test]
async fn a_different_model_drops_a_signature_it_cannot_replay() {
    let source_reference = model("test/strict");
    let target_reference = model("test/other");
    let conversation = exported(&source_reference, false).await;

    let target = strict(target_reference);
    let engine = engine(target.clone(), Arc::new(InMemoryStore::new()));
    let imported = engine.import(conversation).await.expect("import");
    let outcome = engine
        .send(imported.id, Message::user("go on"))
        .finish()
        .await
        .expect("a foreign signature is dropped, not fatal");
    assert_eq!(outcome.session.phase, Phase::Idle);

    let request = target.last_request().expect("request");
    assert_eq!(
        carried_tokens(&request),
        0,
        "the token is dropped before the request is built"
    );
    assert_eq!(
        request.messages[2]
            .reasoning()
            .first()
            .map(|reasoning| reasoning.text.as_str()),
        Some("Rayleigh scattering."),
        "only the signature goes; the reasoning text stays"
    );
}

#[tokio::test]
async fn a_model_switch_mid_tool_loop_is_blocked_instead_of_dropping_the_signature() {
    let source_reference = model("test/strict");
    let target_reference = model("test/other");
    let conversation = exported(&source_reference, true).await;

    let engine = engine(strict(target_reference), Arc::new(InMemoryStore::new()));
    let imported = engine.import(conversation).await.expect("import");
    let error = engine
        .send(imported.id, Message::user("go on"))
        .finish()
        .await
        .expect_err("dropping the signature here would make the vendor reject the turn");

    assert!(
        matches!(error, Error::ContinuityBlocked(_)),
        "a replay that cannot be satisfied is blocked, got {error:?}"
    );
    assert_eq!(
        engine.session(imported.id).await.expect("session").phase,
        Phase::Streaming,
        "the failed turn stays resumable (FR-ERR-5)"
    );
}
