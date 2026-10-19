use norte_core::{
    Capabilities, Modality, Model, ModelRef, ProviderId, StreamingGranularity, ToolKind, WireFamily,
};
use norte_http::{AuthScheme, VendorProfile};
use norte_openai::{OpenAiChat, WIRE_CHAT, capabilities_from_json};

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

fn openrouter(model_id: &str, api_key: &str) -> OpenAiChat {
    let provider = ProviderId::new("openrouter").expect("provider id");
    let reference = ModelRef::new(provider.clone(), model_id.parse().expect("model id"));
    let profile = VendorProfile::new(
        provider,
        WireFamily::new(WIRE_CHAT).expect("wire family"),
        OPENROUTER_BASE,
    )
    .expect("profile")
    .with_auth(AuthScheme::bearer(api_key))
    .with_header("HTTP-Referer", "https://github.com/JeanVydes/Norte")
    .with_header("X-Title", "Norte");

    let capabilities = Capabilities::builder()
        .input(Modality::Text)
        .output(Modality::Text)
        .streaming(StreamingGranularity::Delta)
        .tool_kind(ToolKind::Function)
        .max_context(200_000)
        .build();

    OpenAiChat::new(reference, profile, capabilities).expect("model")
}

#[test]
fn an_openrouter_model_is_an_openai_chat_profile() {
    let model = openrouter("anthropic/claude-sonnet-4.5", "sk-or-v1-test");

    assert_eq!(model.reference().provider.as_str(), "openrouter");
    assert_eq!(
        model.reference().model.as_str(),
        "anthropic/claude-sonnet-4.5"
    );

    let endpoint = model
        .profile()
        .endpoint("chat/completions")
        .expect("endpoint");
    assert_eq!(
        endpoint.as_str(),
        "https://openrouter.ai/api/v1/chat/completions"
    );

    let headers = model.profile().auth.headers().expect("auth headers");
    assert_eq!(headers[0].0.as_str(), "authorization");
    assert_eq!(
        headers[0].1.to_str().expect("value"),
        "Bearer sk-or-v1-test"
    );
}

#[test]
fn host_json_becomes_the_capability_descriptor() {
    let capabilities = capabilities_from_json(&serde_json::json!({
        "modalities": { "input": { "text": {} }, "output": ["text"] },
        "reasoning": { "supported": true, "can_disable": true, "supports_budget": false },
        "tool_kinds": ["function"],
        "features": ["parallel_tool_calls"],
        "streaming": "delta",
        "cache": "none",
        "max_context": 200_000,
        "max_output": 32000
    }))
    .expect("capabilities");

    assert_eq!(capabilities.max_context, Some(200_000));
    assert!(capabilities.supports(ToolKind::Function));
}
