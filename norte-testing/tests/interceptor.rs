//! Turn interceptor tests: a host message that reaches the request in flight.

use std::sync::Arc;
use std::sync::Mutex;

use norte_core::{
    CallId, Engine, EngineBuilder, FnExecutor, InterceptionContext, Message, Result, Role, Store,
    ToolRegistry, TurnInterceptor,
};
use norte_testing::{InMemoryBlobStore, InMemoryStore, ScriptedModel, model};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EchoArgs {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct EchoResult {
    echoed: String,
}

#[norte_core::tool(name = "echo", description = "Echo the provided text")]
struct Echo;

#[async_trait::async_trait]
impl FnExecutor<EchoArgs, EchoResult> for Echo {
    async fn call(&self, args: EchoArgs) -> Result<EchoResult> {
        Ok(EchoResult { echoed: args.text })
    }
}

/// An interceptor that hands over one message on its second consultation.
///
/// The engine consults it before every model request of a turn, so the second
/// consultation is the one that precedes the request the model makes after the
/// tool ran.
struct SteerOnSecondCall {
    calls: Mutex<usize>,
}

impl SteerOnSecondCall {
    fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

#[async_trait::async_trait]
impl TurnInterceptor for SteerOnSecondCall {
    fn name(&self) -> &'static str {
        "steer-on-second-call"
    }

    async fn intercept(&self, _context: &InterceptionContext<'_>) -> Result<Vec<Message>> {
        let mut calls = self.calls.lock().expect("lock");
        *calls += 1;
        if *calls == 2 {
            Ok(vec![Message::user("steer")])
        } else {
            Ok(Vec::new())
        }
    }
}

fn engine_with(
    scripted: Arc<ScriptedModel>,
    interceptor: Arc<dyn TurnInterceptor>,
) -> (Engine, Arc<InMemoryStore>) {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let store = Arc::new(InMemoryStore::new());
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted)
        .tools(tools)
        .interceptors(vec![interceptor])
        .build()
        .expect("build engine");
    (engine, store)
}

#[tokio::test]
async fn an_intercepted_message_reaches_the_request_in_flight() {
    let call_id = CallId::new();
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_tool_call(call_id, "echo", &serde_json::json!({ "text": "hi" }))
            .turn_text("all done"),
    );
    let (engine, store) = engine_with(scripted.clone(), Arc::new(SteerOnSecondCall::new()));

    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("please echo"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(scripted.request_count(), 2, "the turn made two requests");

    let requests = scripted.requests();
    let first: Vec<String> = requests[0].messages.iter().map(Message::text).collect();
    assert_eq!(
        first,
        ["please echo"],
        "the first request carries nothing from the interceptor"
    );
    let second = &requests[1].messages;
    assert_eq!(
        second.last().map(Message::text).as_deref(),
        Some("steer"),
        "the intercepted message reaches the request in flight"
    );
    let second_users: Vec<String> = second
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(
        second_users,
        ["please echo", "steer"],
        "and it lands after the message that started the turn"
    );

    let history = store.history(outcome.session.head).await.expect("history");
    let roles: Vec<Role> = history.iter().map(|message| message.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::Tool,
            Role::User,
            Role::Assistant
        ],
        "the intercepted message lands after the tool result"
    );
    assert_eq!(history[3].text(), "steer");
    assert_eq!(
        outcome.message.as_ref().map(Message::text).as_deref(),
        Some("all done")
    );
}

#[tokio::test]
async fn an_interceptor_that_answers_nothing_leaves_the_turn_untouched() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello"));
    let (engine, store) = engine_with(scripted.clone(), Arc::new(SteerOnSecondCall::new()));

    let session = engine.create().await.expect("create");
    let outcome = engine
        .send(session.id, Message::user("ping"))
        .finish()
        .await
        .expect("turn");

    assert_eq!(scripted.request_count(), 1);
    let history = store.history(outcome.session.head).await.expect("history");
    assert_eq!(history.len(), 2, "nothing was appended");
    assert_eq!(history[0].role, Role::User);
    assert_eq!(history[1].role, Role::Assistant);
}

#[tokio::test]
async fn a_resume_without_input_drains_the_steering_queue() {
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_text("first")
            .turn_text("second"),
    );
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Echo).expect("register echo");
    let store = Arc::new(InMemoryStore::new());
    let engine = EngineBuilder::new()
        .store(store.clone())
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(scripted.clone())
        .tools(tools)
        .build()
        .expect("build engine");

    let session = engine.create().await.expect("create");
    engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");

    engine
        .steer(session.id, Message::user("a"))
        .await
        .expect("steer");
    engine
        .steer(session.id, Message::user("b"))
        .await
        .expect("steer");

    let outcome = engine.resume(session.id).finish().await.expect("resume");

    let requests = scripted.requests();
    assert_eq!(requests.len(), 2, "the resume made one request");
    let users: Vec<String> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(
        users,
        ["go", "a", "b"],
        "both queued messages reach the turn, in order"
    );

    let history = store.history(outcome.session.head).await.expect("history");
    let texts: Vec<String> = history
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(texts, ["go", "a", "b"]);
}
