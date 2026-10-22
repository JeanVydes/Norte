//! Turn interceptor tests: a host message that reaches the request in flight.

use std::sync::Arc;
use std::sync::Mutex;

use norte_core::{
    CallId, Engine, EngineBuilder, FnExecutor, Interception, InterceptionContext, Message, Result,
    Role, Store, ToolRegistry, TurnInterceptor,
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

/// An interceptor that speaks once the tools of the turn have answered.
///
/// The decision is taken from the conversation, not from a counter: the host
/// adds its message when the last thing that happened was a tool result, which
/// is the point where the request is about to be built.
struct SteerAfterTools {
    injected: Mutex<bool>,
}

impl SteerAfterTools {
    fn new() -> Self {
        Self {
            injected: Mutex::new(false),
        }
    }
}

#[async_trait::async_trait]
impl TurnInterceptor for SteerAfterTools {
    fn name(&self) -> &'static str {
        "steer-after-tools"
    }

    async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception> {
        let after_tools = context
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Tool);
        if !after_tools {
            return Ok(Interception::Continue);
        }
        let mut injected = self.injected.lock().expect("lock");
        if *injected {
            return Ok(Interception::Continue);
        }
        *injected = true;
        Ok(Interception::Inject(vec![Message::user("steer")]))
    }
}

/// An interceptor that reads the conversation and says nothing.
struct Reading {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl TurnInterceptor for Reading {
    fn name(&self) -> &'static str {
        "reading"
    }

    async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception> {
        let texts: Vec<String> = context.messages.iter().map(Message::text).collect();
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(texts.join("|"));
        Ok(Interception::Continue)
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
    let (engine, store) = engine_with(scripted.clone(), Arc::new(SteerAfterTools::new()));

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
    let (engine, store) = engine_with(scripted.clone(), Arc::new(SteerAfterTools::new()));

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
async fn an_interceptor_reads_the_history_the_request_carries() {
    let scripted = Arc::new(ScriptedModel::new(model("test/echo")).turn_text("hello"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (engine, _store) = engine_with(
        scripted.clone(),
        Arc::new(Reading {
            seen: Arc::clone(&seen),
        }),
    );

    let session = engine.create().await.expect("create");
    engine
        .send(session.id, Message::user("ping"))
        .finish()
        .await
        .expect("turn");

    let reads = seen
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert!(!reads.is_empty(), "the interceptor was consulted");
    assert_eq!(
        reads[0], "",
        "the first consultation comes before the input joins the session"
    );
    assert_eq!(
        reads[1], "ping",
        "and the request carries the conversation the interceptor saw"
    );
}

/// An interceptor that speaks when the turn is about to end.
struct SteerAtTheEnd {
    injected: Mutex<bool>,
}

impl SteerAtTheEnd {
    fn new() -> Self {
        Self {
            injected: Mutex::new(false),
        }
    }
}

#[async_trait::async_trait]
impl TurnInterceptor for SteerAtTheEnd {
    fn name(&self) -> &'static str {
        "steer-at-the-end"
    }

    async fn intercept(&self, context: &InterceptionContext<'_>) -> Result<Interception> {
        let ending = context
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Assistant);
        if !ending {
            return Ok(Interception::Continue);
        }
        let mut injected = self.injected.lock().expect("lock");
        if *injected {
            return Ok(Interception::Continue);
        }
        *injected = true;
        Ok(Interception::Inject(vec![Message::user("one more thing")]))
    }
}

#[tokio::test]
async fn a_message_the_host_holds_keeps_the_turn_alive() {
    let scripted = Arc::new(
        ScriptedModel::new(model("test/echo"))
            .turn_text("first answer")
            .turn_text("second answer"),
    );
    let (engine, _store) = engine_with(scripted.clone(), Arc::new(SteerAtTheEnd::new()));

    let session = engine.create().await.expect("create");
    engine
        .send(session.id, Message::user("go"))
        .finish()
        .await
        .expect("turn");

    let requests = scripted.requests();
    assert_eq!(requests.len(), 2, "the turn asked again");
    let users: Vec<String> = requests[1]
        .messages
        .iter()
        .filter(|message| message.role == Role::User)
        .map(Message::text)
        .collect();
    assert_eq!(
        users,
        ["go", "one more thing"],
        "the message the host held reached the request that followed"
    );
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
