//! Per-turn request options: parameter overlay, tool subsetting and audit
//! (FR-ENG-7, FR-ENG-8, FR-ENG-9, FR-TEST-6).
//!
//! These are the exhaustive vectors: every generation parameter, every reasoning
//! mode and representative tool subsets, asserted on the reconstructed request
//! the adapter would have received.

use std::sync::Arc;
use std::time::Duration;

use rustychain_core::{
    Engine, EngineBuilder, EngineConfig, EngineEvent, Error, FnExecutor, GenerationParams, Message,
    Model, ModelRequest, Observer, ReasoningEffort, ReasoningRequest, Result, ToolName,
    ToolRegistry, TurnOptions,
};
use rustychain_testing::{
    InMemoryBlobStore, InMemoryStore, ProbeModel, RecordingObserver, ScriptedModel, model,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    text: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Out {
    text: String,
}

#[rustychain_core::tool(name = "alpha", description = "alpha tool")]
struct Alpha;

#[rustychain_core::tool(name = "beta", description = "beta tool")]
struct Beta;

#[rustychain_core::tool(name = "gamma", description = "gamma tool")]
struct Gamma;

#[async_trait::async_trait]
impl FnExecutor<Args, Out> for Alpha {
    async fn call(&self, args: Args) -> Result<Out> {
        Ok(Out { text: args.text })
    }
}

#[async_trait::async_trait]
impl FnExecutor<Args, Out> for Beta {
    async fn call(&self, args: Args) -> Result<Out> {
        Ok(Out { text: args.text })
    }
}

#[async_trait::async_trait]
impl FnExecutor<Args, Out> for Gamma {
    async fn call(&self, args: Args) -> Result<Out> {
        Ok(Out { text: args.text })
    }
}

fn registry() -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.register_declarator(&Alpha).expect("alpha");
    tools.register_declarator(&Beta).expect("beta");
    tools.register_declarator(&Gamma).expect("gamma");
    tools
}

fn probe() -> Arc<ProbeModel> {
    Arc::new(ProbeModel::new(Arc::new(
        ScriptedModel::new(model("test/echo")).turn_text("unused"),
    )))
}

fn build(
    probe: Arc<ProbeModel>,
    tools: ToolRegistry,
    observers: Vec<Arc<dyn Observer>>,
    config: EngineConfig,
) -> Engine {
    EngineBuilder::new()
        .store(Arc::new(InMemoryStore::new()))
        .blobs(Arc::new(InMemoryBlobStore::new()))
        .model(probe as Arc<dyn Model>)
        .tools(tools)
        .observers(observers)
        .config(config)
        .build()
        .expect("engine")
}

async fn request_for(engine: &Engine, probe: &ProbeModel, options: TurnOptions) -> ModelRequest {
    let session = engine.create().await.expect("create");
    engine
        .turn(session.id)
        .input(Message::user("hi"))
        .options(options)
        .start()
        .finish()
        .await
        .expect("turn");
    probe.last_request().expect("request")
}

fn names(request: &ModelRequest) -> Vec<String> {
    request
        .tools
        .iter()
        .map(|spec| spec.name.as_str().to_owned())
        .collect()
}

fn name(value: &str) -> ToolName {
    ToolName::new(value).expect("tool name")
}

async fn wait_for_event(
    recorder: &RecordingObserver,
    predicate: impl Fn(&EngineEvent) -> bool,
) -> EngineEvent {
    for _ in 0..500 {
        if let Some(event) = recorder.events().into_iter().find(|event| predicate(event)) {
            return event;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("the event never arrived");
}

#[tokio::test]
async fn parameters_overlay_per_field() {
    let probe = probe();
    let defaults = EngineConfig {
        params: GenerationParams {
            top_k: Some(40),
            top_p: Some(0.5),
            ..GenerationParams::default()
        },
        ..EngineConfig::default()
    };
    let engine = build(probe.clone(), registry(), Vec::new(), defaults.clone());

    // Only temperature is set on the turn: top_k and top_p are inherited.
    let request = request_for(
        &engine,
        &probe,
        TurnOptions {
            params: GenerationParams {
                temperature: Some(1.0),
                ..GenerationParams::default()
            },
            ..TurnOptions::default()
        },
    )
    .await;
    assert_eq!(request.params.temperature, Some(1.0));
    assert_eq!(request.params.top_k, Some(40));
    assert_eq!(request.params.top_p, Some(0.5));
}

#[tokio::test]
async fn every_parameter_vector_is_reflected() {
    let defaults = GenerationParams {
        top_k: Some(40),
        top_p: Some(0.5),
        ..GenerationParams::default()
    };
    let config = EngineConfig {
        params: defaults.clone(),
        ..EngineConfig::default()
    };
    let probe = probe();
    let engine = build(probe.clone(), registry(), Vec::new(), config);

    let vectors = vec![
        GenerationParams::default(),
        GenerationParams {
            temperature: Some(0.0),
            ..GenerationParams::default()
        },
        GenerationParams {
            temperature: Some(2.0),
            ..GenerationParams::default()
        },
        GenerationParams {
            top_p: Some(1.0),
            ..GenerationParams::default()
        },
        GenerationParams {
            top_k: Some(1),
            ..GenerationParams::default()
        },
        GenerationParams {
            max_output_tokens: Some(1),
            ..GenerationParams::default()
        },
        GenerationParams {
            stop_sequences: vec!["stop".to_owned()],
            ..GenerationParams::default()
        },
        GenerationParams {
            seed: Some(0),
            ..GenerationParams::default()
        },
        GenerationParams {
            output_schema: Some(json!({ "type": "object" })),
            ..GenerationParams::default()
        },
        GenerationParams {
            temperature: Some(0.3),
            top_p: Some(0.9),
            top_k: Some(50),
            max_output_tokens: Some(4096),
            stop_sequences: vec!["a".to_owned(), "b".to_owned()],
            seed: Some(42),
            output_schema: Some(json!(true)),
        },
    ];

    for vector in vectors {
        let expected = vector.over(&defaults);
        let request = request_for(
            &engine,
            &probe,
            TurnOptions {
                params: vector,
                ..TurnOptions::default()
            },
        )
        .await;
        assert_eq!(
            request.params, expected,
            "a parameter vector was not applied"
        );
    }
}

#[tokio::test]
async fn every_reasoning_mode_is_reflected() {
    let probe = probe();
    let engine = build(
        probe.clone(),
        registry(),
        Vec::new(),
        EngineConfig::default(),
    );

    let modes = [
        ReasoningRequest::Disabled,
        ReasoningRequest::Automatic,
        ReasoningRequest::Effort(ReasoningEffort::Minimal),
        ReasoningRequest::Effort(ReasoningEffort::High),
        ReasoningRequest::Budget(1000),
    ];
    for mode in modes {
        let request = request_for(
            &engine,
            &probe,
            TurnOptions {
                reasoning: Some(mode),
                ..TurnOptions::default()
            },
        )
        .await;
        assert_eq!(request.reasoning, mode);
    }

    // No override inherits the engine default.
    let inherited = request_for(&engine, &probe, TurnOptions::default()).await;
    assert_eq!(inherited.reasoning, ReasoningRequest::Automatic);
}

#[tokio::test]
async fn tool_subset_is_canonical_and_deduplicated() {
    let probe = probe();
    let engine = build(
        probe.clone(),
        registry(),
        Vec::new(),
        EngineConfig::default(),
    );

    let all = request_for(&engine, &probe, TurnOptions::default()).await;
    assert_eq!(names(&all), vec!["alpha", "beta", "gamma"]);

    let subset = request_for(
        &engine,
        &probe,
        TurnOptions {
            tools: Some(vec![name("gamma"), name("alpha")]),
            ..TurnOptions::default()
        },
    )
    .await;
    assert_eq!(names(&subset), vec!["alpha", "gamma"]);

    let deduped = request_for(
        &engine,
        &probe,
        TurnOptions {
            tools: Some(vec![name("alpha"), name("alpha")]),
            ..TurnOptions::default()
        },
    )
    .await;
    assert_eq!(names(&deduped), vec!["alpha"]);

    let empty = request_for(
        &engine,
        &probe,
        TurnOptions {
            tools: Some(Vec::new()),
            ..TurnOptions::default()
        },
    )
    .await;
    assert!(empty.tools.is_empty());
}

#[tokio::test]
async fn an_unknown_tool_is_a_typed_error() {
    let probe = probe();
    let engine = build(
        probe.clone(),
        registry(),
        Vec::new(),
        EngineConfig::default(),
    );
    let session = engine.create().await.expect("create");

    let error = engine
        .turn(session.id)
        .input(Message::user("hi"))
        .tools([name("nope")])
        .start()
        .finish()
        .await
        .expect_err("unknown tool must fail");
    assert!(matches!(error, Error::ToolNotFound(_)));
}

#[tokio::test]
async fn the_audit_event_reports_the_effective_config() {
    let recorder = Arc::new(RecordingObserver::new("audit"));
    let probe = probe();
    let engine = build(
        probe.clone(),
        registry(),
        vec![recorder.clone()],
        EngineConfig::default(),
    );
    let session = engine.create().await.expect("create");

    engine
        .turn(session.id)
        .input(Message::user("hi"))
        .temperature(0.9)
        .top_k(3)
        .reasoning(ReasoningRequest::Disabled)
        .tools([name("alpha")])
        .start()
        .finish()
        .await
        .expect("turn");

    let event = wait_for_event(&recorder, |event| {
        matches!(event, EngineEvent::RequestReconstructed { .. })
    })
    .await;
    let EngineEvent::RequestReconstructed {
        params,
        reasoning,
        tools,
        ..
    } = event
    else {
        unreachable!("filtered above");
    };
    assert_eq!(params.temperature, Some(0.9));
    assert_eq!(params.top_k, Some(3));
    assert_eq!(reasoning, ReasoningRequest::Disabled);
    assert_eq!(tools, vec![name("alpha")]);
}

#[tokio::test]
async fn export_captures_and_import_restores_request_config() {
    let config = EngineConfig {
        params: GenerationParams {
            top_p: Some(0.5),
            ..GenerationParams::default()
        },
        ..EngineConfig::default()
    };
    let options = TurnOptions {
        params: GenerationParams {
            temperature: Some(1.0),
            top_k: Some(7),
            ..GenerationParams::default()
        },
        reasoning: Some(ReasoningRequest::Effort(ReasoningEffort::High)),
        tools: Some(vec![name("beta")]),
    };
    let expected = options.over(&TurnOptions {
        params: config.params.clone(),
        reasoning: Some(config.reasoning),
        tools: None,
    });

    let probe_a = probe();
    let engine_a = build(probe_a.clone(), registry(), Vec::new(), config.clone());
    let session = engine_a.create().await.expect("create");
    engine_a
        .append_history(
            session.id,
            vec![Message::system("sys"), Message::user("q1")],
        )
        .await
        .expect("seed");
    engine_a
        .turn(session.id)
        .input(Message::user("q2"))
        .options(options)
        .start()
        .finish()
        .await
        .expect("turn");

    let conversation = engine_a.export(session.id).await.expect("export");
    assert_eq!(conversation.options, Some(expected));
    assert!(
        !conversation
            .metadata
            .contains_key("rustychain.turn_options"),
        "the reserved key must be lifted out of host metadata"
    );

    // A fresh, identically configured engine imports and inherits the config.
    let probe_b = probe();
    let engine_b = build(probe_b.clone(), registry(), Vec::new(), config);
    let imported = engine_b
        .import(
            rustychain_core::Conversation::from_json(&conversation.to_json().expect("json"))
                .expect("decode"),
        )
        .await
        .expect("import");

    // Neither host passes options: both must inherit the restored config.
    engine_a
        .turn(session.id)
        .input(Message::user("q3"))
        .start()
        .finish()
        .await
        .expect("send a");
    engine_b
        .turn(imported.id)
        .input(Message::user("q3"))
        .start()
        .finish()
        .await
        .expect("send b");

    let request_a = probe_a.last_request().expect("request a");
    let request_b = probe_b.last_request().expect("request b");
    assert_eq!(request_b.params.temperature, Some(1.0));
    assert_eq!(request_b.params.top_k, Some(7));
    assert_eq!(request_b.params.top_p, Some(0.5));
    assert_eq!(
        request_b.reasoning,
        ReasoningRequest::Effort(ReasoningEffort::High)
    );
    assert_eq!(names(&request_b), vec!["beta"]);
    assert_eq!(
        serde_json::to_string(&request_a).expect("bytes a"),
        serde_json::to_string(&request_b).expect("bytes b"),
        "the complete request must be reproduced byte-identically"
    );
}
