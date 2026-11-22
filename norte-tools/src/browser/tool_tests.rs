use norte_core::Error;

use super::BrowserTool;

#[test]
fn the_http_client_is_built_on_first_use() {
    let tool = BrowserTool::new();
    assert!(tool.client.get().is_none());

    assert!(tool.client().is_ok());
    assert!(tool.client.get().is_some());
}

#[test]
fn a_client_build_failure_becomes_a_typed_error() {
    let tool = BrowserTool::new();
    let _ = tool.client.set(Err("no TLS backend".to_owned()));

    let error = tool.client().expect_err("client error");
    assert!(matches!(error, Error::Other(reason) if reason.contains("no TLS backend")));
}
