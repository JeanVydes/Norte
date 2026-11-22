use std::sync::Arc;
use std::time::Duration;

use norte_core::{Digest, FnExecutor, ToolContext};
use serde_json::json;
use tokio::sync::{Mutex, broadcast};

use super::cdp::{Tab, encode_query, pick_tab, wait_for_event};
use super::dom::{check_snapshot, key_event, snapshot_token};
use super::error::{BrowserErrorCode, is_connection_error, tool_fail_code};
use super::render::describe;
use super::*;

fn tab(id: &str, title: &str, url: &str) -> Tab {
    Tab {
        id: id.to_owned(),
        title: title.to_owned(),
        url: url.to_owned(),
        ws_url: format!("ws://127.0.0.1:9222/devtools/page/{id}"),
    }
}

fn element(id: usize, tag: &str, text: &str) -> InteractiveElement {
    InteractiveElement {
        id,
        tag: tag.to_owned(),
        text: text.to_owned(),
        ..InteractiveElement::default()
    }
}

#[test]
fn the_model_request_does_not_carry_host_configuration() {
    let declaration = BrowserTool::new().into_declaration();
    let schema = serde_json::to_value(&declaration.parameters).expect("schema");
    let top = schema["properties"].as_object().expect("properties");
    assert!(!top.contains_key("endpoint"), "{schema}");
    assert!(!top.contains_key("max_elements"), "{schema}");
    assert!(!top.contains_key("max_page_chars"), "{schema}");
    assert!(top.contains_key("snapshot"), "{schema}");

    let request: BrowserRequest =
        serde_json::from_value(json!({"action": {"type": "extract_dom"}})).expect("parses");
    assert!(
        !request.highlight,
        "highlight changes the page, so it is opt-in"
    );
    assert!(request.snapshot.is_none());
}

#[test]
fn the_extract_dom_schema_exposes_paging_without_exposing_limits() {
    let declaration = BrowserTool::new().into_declaration();
    let schema = serde_json::to_value(&declaration.parameters).expect("schema");
    let variants = schema["$defs"]["BrowserAction"]["oneOf"]
        .as_array()
        .expect("action variants");
    let extract = variants
        .iter()
        .find(|variant| variant["properties"]["type"]["const"] == "extract_dom")
        .expect("extract_dom variant");
    let properties = extract["properties"].as_object().expect("properties");
    assert!(properties.contains_key("offset"), "{extract}");
    assert!(properties.contains_key("filter"), "{extract}");
    assert!(!properties.contains_key("endpoint"), "{extract}");
}

#[test]
fn the_endpoint_can_be_configured_only_on_the_tool() {
    let limits = BrowserLimits::default().with_endpoint("http://127.0.0.1:9333/");
    assert_eq!(limits.endpoint, "http://127.0.0.1:9333");
    let tool = BrowserTool::new().with_endpoint("http://127.0.0.1:9444");
    assert_eq!(tool.endpoint(), "http://127.0.0.1:9444");
}

#[test]
fn the_url_policy_allows_web_pages_but_not_local_files_by_default() {
    let limits = BrowserLimits::default();
    assert!(limits.validate_url("https://example.com/path").is_ok());
    assert!(
        limits.validate_url("file:///etc/passwd").is_err(),
        "a model must not read local files through the browser"
    );
    assert!(limits.validate_url("javascript:alert(1)").is_err());
}

#[test]
fn the_url_policy_can_opt_into_files_and_refuse_a_host() {
    let policy = UrlPolicy::new()
        .allow_scheme(BrowserScheme::File)
        .deny_host("internal.example.com")
        .expect("valid host");
    let limits = BrowserLimits::default().with_url_policy(policy);
    assert!(limits.validate_url("file:///tmp/page.html").is_ok());
    assert!(limits.validate_url("https://public.example.com").is_ok());
    assert!(limits.validate_url("https://internal.example.com").is_err());
}

#[test]
fn a_refused_url_carries_the_policy_error_code() {
    let error = BrowserLimits::default()
        .validate_url("file:///etc/passwd")
        .expect_err("the policy refuses files");
    assert!(matches!(
        &error,
        norte_core::Error::ToolFailed {
            code: Some(code),
            ..
        } if code == "url_policy"
    ));
    assert!(!is_connection_error(&error));
}

#[test]
fn a_dead_socket_is_classified_without_reading_the_error_text() {
    let error = tool_fail_code("gone", BrowserErrorCode::CdpClosed);
    assert!(is_connection_error(&error));
    let navigation = tool_fail_code("refused", BrowserErrorCode::Navigation);
    assert!(!is_connection_error(&navigation));
}

#[tokio::test]
async fn waiting_for_a_load_event_ignores_unrelated_events() {
    let (sender, mut events) = broadcast::channel(8);
    sender
        .send(json!({ "method": "Page.lifecycleEvent", "params": { "name": "init" } }))
        .expect("receiver is alive");
    sender
        .send(json!({
            "method": "Page.lifecycleEvent",
            "params": { "loaderId": "loader-1", "name": "load" }
        }))
        .expect("receiver is alive");
    wait_for_event(
        &mut events,
        "Page.lifecycleEvent",
        |params| params["loaderId"] == "loader-1" && params["name"] == "load",
        Duration::from_millis(100),
    )
    .await
    .expect("the matching event arrived");
}

#[test]
fn a_tab_substring_is_matched_without_regard_to_case() {
    let tabs = vec![tab("AAA", "The Guide", "https://docs.example.com/guide")];
    let picked = pick_tab(&tabs, None, Some("DOCS.EXAMPLE")).expect("case is ignored");
    assert_eq!(picked.id, "AAA");
}

#[test]
fn the_new_tab_route_encodes_the_url_as_one_query_value() {
    let encoded = encode_query("https://example.com/a?x=1&y=2#frag");
    assert_eq!(encoded, "https://example.com/a%3Fx%3D1%26y%3D2%23frag");
    assert!(!encoded.contains('#'));
}

#[test]
fn a_key_is_mapped_to_a_complete_cdp_event() {
    let params = key_event(BrowserKey::Enter);
    assert_eq!(params["key"], "Enter");
    assert_eq!(params["code"], "Enter");
    assert_eq!(params["windowsVirtualKeyCode"], 13);

    let params = key_event(BrowserKey::Space);
    assert_eq!(params["key"], " ");
    assert_eq!(params["windowsVirtualKeyCode"], 32);
}

#[test]
fn a_press_key_request_parses_with_a_typed_key_and_snapshot() {
    let request: BrowserRequest = serde_json::from_value(json!({
        "action": {"type": "press_key", "key": "escape", "agent_id": 3},
        "snapshot": "snapshot-token"
    }))
    .expect("parses");
    assert_eq!(
        request.action,
        BrowserAction::PressKey {
            key: BrowserKey::Escape,
            selector: None,
            agent_id: Some(3),
        }
    );
    assert_eq!(request.snapshot.as_deref(), Some("snapshot-token"));
}

#[test]
fn a_new_offset_invalidates_the_element_snapshot() {
    let fingerprint = "BUTTON:Sign in";
    assert_ne!(
        snapshot_token(fingerprint, 0),
        snapshot_token(fingerprint, 10)
    );
}

#[tokio::test]
async fn an_agent_id_without_the_matching_snapshot_is_rejected() {
    let slot = Arc::new(Mutex::new(Some("current".to_owned())));
    assert!(check_snapshot(&slot, None).await.is_err());
    assert!(check_snapshot(&slot, Some("stale")).await.is_err());
    assert!(check_snapshot(&slot, Some("current")).await.is_ok());
}

#[test]
fn the_text_answer_marks_page_text_and_reports_paging() {
    let output = BrowserOutput {
        url: "https://example.com".to_owned(),
        title: "Example".to_owned(),
        message: "Read the page".to_owned(),
        elements: vec![element(2, "BUTTON", "Sign in")],
        elements_matched: 5,
        elements_offset: 2,
        dom_snapshot: Some("snapshot-token".to_owned()),
        page_text: Some("Welcome".to_owned()),
        page_text_origin: Some(ContentOrigin::UntrustedPage),
        screenshot_base64: None,
        tabs: vec![BrowserTab {
            id: "AAA".to_owned(),
            title: "Example".to_owned(),
            url: "https://example.com".to_owned(),
            current: true,
        }],
        request_digest: Digest::of(b"request"),
    };
    let text = describe(&output);
    assert!(text.contains("untrusted page text"), "{text}");
    assert!(text.contains("* AAA"), "{text}");
    assert!(text.contains("2|BUTTON|Sign in"), "{text}");
    assert!(text.contains("snapshot: snapshot-token"), "{text}");
    assert!(text.contains("offset 3 to continue"), "{text}");
}

#[tokio::test]
#[ignore = "needs a browser with --remote-debugging-port=9222"]
async fn the_tool_reads_a_live_page_under_a_file_policy() {
    let policy = UrlPolicy::new().allow_scheme(BrowserScheme::File);
    let tool = BrowserTool::new().with_url_policy(policy);
    let request: BrowserRequest = serde_json::from_value(json!({
        "action": {"type": "read_page"}
    }))
    .expect("parses");
    let output = tool
        .call_with_context(&ToolContext::new(), request)
        .await
        .expect("the browser answers");
    let structured = output.structured.expect("structured answer");
    assert!(!structured["page_text"].is_null(), "the page text arrived");
}
