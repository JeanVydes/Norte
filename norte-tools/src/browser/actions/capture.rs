use std::time::Duration;

use norte_core::Result;
use serde_json::json;

use super::super::cdp::CdpSession;
use super::super::dom::{extract_interactive, read_page as read_page_script};
use super::super::error::tool_fail;
use super::super::limits::BrowserLimits;
use super::super::outcome::PageOutcome;
use super::super::output::{ContentOrigin, InteractiveElement};

/// Reads the controls left by a page action without failing the whole call.
pub(in crate::browser) async fn current_elements(
    session: &CdpSession,
    limits: &BrowserLimits,
) -> Vec<InteractiveElement> {
    let cap = limits.max_elements;
    extract_interactive(session, cap, 0, None, limits.command_timeout())
        .await
        .map(|snapshot| snapshot.elements)
        .unwrap_or_default()
}

/// Maps the visible controls to ids, with filtering and paging done in the page.
pub(in crate::browser) async fn extract_dom(
    session: &CdpSession,
    limits: &BrowserLimits,
    requested_cap: Option<usize>,
    offset: Option<usize>,
    filter: Option<&str>,
) -> Result<PageOutcome> {
    let cap = requested_cap
        .unwrap_or(limits.max_elements)
        .min(limits.max_elements);
    let offset = offset.unwrap_or_default();
    let snapshot =
        extract_interactive(session, cap, offset, filter, limits.command_timeout()).await?;
    let shown = snapshot.elements.len();
    Ok(PageOutcome {
        message: format!(
            "Extracted {shown} of {} matching controls from offset {offset}",
            snapshot.matched
        ),
        elements: snapshot.elements,
        elements_matched: snapshot.matched,
        elements_offset: offset,
        dom_snapshot: Some(snapshot.token),
        ..PageOutcome::default()
    })
}

/// Reads page text and marks it as untrusted in both answer shapes.
pub(in crate::browser) async fn read_page(
    session: &CdpSession,
    limits: &BrowserLimits,
    selector: Option<&str>,
    max_chars: Option<usize>,
) -> Result<PageOutcome> {
    let cap = max_chars
        .unwrap_or(limits.max_page_chars)
        .min(limits.max_page_chars);
    let text = read_page_script(session, selector, cap, limits.command_timeout()).await?;
    Ok(PageOutcome {
        message: format!("Read {} characters from the page", text.len()),
        page_text: Some(text),
        page_text_origin: Some(ContentOrigin::UntrustedPage),
        ..PageOutcome::default()
    })
}

/// Captures the viewport as base64 PNG. The text answer reports only its size.
pub(in crate::browser) async fn screenshot(
    session: &CdpSession,
    timeout: Duration,
) -> Result<PageOutcome> {
    let response = session
        .send(
            "Page.captureScreenshot",
            json!({ "format": "png" }),
            timeout,
        )
        .await?;
    let data = response["data"]
        .as_str()
        .ok_or_else(|| tool_fail("Failed to extract screenshot payload".to_owned()))?;
    Ok(PageOutcome {
        message: "Screenshot captured successfully".to_owned(),
        screenshot: Some(data.to_owned()),
        ..PageOutcome::default()
    })
}
