//! Requests enforce declared capabilities and wire quirks before any I/O.

use norte_anthropic::{AnthropicMessages, WIRE_MESSAGES};
use norte_core::{
    Capabilities, Error, GenerationSupport, Message, ModelRef, ModelRequest, ParameterSupport,
    StructuredOutputMode, SupportedParam,
};
use norte_http::{Quirk, VendorProfile};

fn adapter(capabilities: Capabilities) -> (AnthropicMessages, ModelRef) {
    let model = ModelRef::new("example".parse().unwrap(), "model".parse().unwrap());
    let profile = VendorProfile::new(
        model.provider.clone(),
        WIRE_MESSAGES.parse().unwrap(),
        "https://example.invalid",
    )
    .unwrap()
    .with_quirk(Quirk::NoPromptCacheBreakpoints);
    (
        AnthropicMessages::new(model.clone(), profile, capabilities).unwrap(),
        model,
    )
}

#[test]
fn complete_declarations_reject_unsupported_parameters_before_encoding() {
    let generation = GenerationSupport {
        supported: [SupportedParam::TopP].into(),
        complete: true,
    };
    assert_eq!(
        generation.support(SupportedParam::TopP),
        ParameterSupport::Supported
    );
    assert_eq!(
        generation.support(SupportedParam::Seed),
        ParameterSupport::Unsupported
    );
    let (adapter, model) = adapter(Capabilities::builder().generation(generation).build());
    let mut request = ModelRequest::new(model, vec![Message::user("hello")]);
    request.params.temperature = Some(0.5);
    assert!(
        matches!(adapter.request_body(&request), Err(Error::Capability { feature, .. }) if feature.contains("Temperature"))
    );
}

#[test]
fn model_support_does_not_make_an_unmapped_wire_parameter_sendable() {
    let generation = GenerationSupport {
        supported: [SupportedParam::Seed].into(),
        complete: true,
    };
    let (adapter, model) = adapter(Capabilities::builder().generation(generation).build());
    let mut request = ModelRequest::new(model, vec![Message::user("hello")]);
    request.params.seed = Some(42);
    assert!(
        matches!(adapter.request_body(&request), Err(Error::Capability { feature, .. }) if feature.contains(WIRE_MESSAGES))
    );
}

#[test]
fn partial_declarations_keep_unknown_sampling_and_quirks_omit_automatic_markers() {
    let caps = Capabilities::builder()
        .structured_output(StructuredOutputMode::JsonSchema)
        .build();
    assert_eq!(
        caps.generation.support(SupportedParam::Temperature),
        ParameterSupport::Unknown
    );
    let (adapter, model) = adapter(caps);
    let mut request = ModelRequest::new(
        model,
        vec![Message::system("rules"), Message::user("hello")],
    );
    request.cache_boundaries = vec![1];
    request.params.temperature = Some(0.5);
    request.params.output_schema =
        Some(serde_json::json!({"type":"object","additionalProperties":false}));
    let body = adapter.request_body(&request).unwrap();
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(body["temperature"], 0.5);
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    request.extensions.insert(
        format!("{WIRE_MESSAGES}:cache_control").parse().unwrap(),
        serde_json::json!({"type":"ephemeral"}),
    );
    assert!(
        matches!(adapter.request_body(&request), Err(Error::Capability { feature, .. }) if feature.contains("cache_control"))
    );
}
