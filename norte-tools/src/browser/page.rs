use std::sync::Arc;
use std::time::Duration;

use norte_core::{Digest, Result};
use tokio::sync::Mutex;

use super::actions::{
    click, extract_dom, navigate, press_key, read_page, screenshot, scroll, target_selector,
    type_text, wait,
};
use super::cdp::CdpSession;
use super::error::tool_fail;
use super::limits::BrowserLimits;
use super::outcome::PageOutcome;
use super::output::BrowserOutput;
use super::request::{BrowserAction, BrowserRequest};

/// Runs a page action and assembles the fields the renderer needs.
pub(super) async fn page_action(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    client: &reqwest::Client,
    session: &CdpSession,
    snapshot_slot: &Arc<Mutex<Option<String>>>,
) -> Result<PageOutcome> {
    let timeout = limits.command_timeout();
    let snapshot = request.snapshot.as_deref();
    match &request.action {
        BrowserAction::Navigate { url } => navigate(session, limits, url).await,
        BrowserAction::Click { selector, agent_id } => {
            let selector = target_selector(
                selector.as_deref(),
                *agent_id,
                snapshot,
                snapshot_slot,
                false,
            )
            .await?;
            click(session, client, limits, &selector, request.highlight).await
        }
        BrowserAction::Type {
            selector,
            agent_id,
            text,
            clear,
        } => {
            let selector = target_selector(
                selector.as_deref(),
                *agent_id,
                snapshot,
                snapshot_slot,
                true,
            )
            .await?;
            type_text(session, &selector, text, *clear, timeout).await
        }
        BrowserAction::Scroll { delta_x, delta_y } => {
            scroll(session, *delta_x, *delta_y, timeout).await
        }
        BrowserAction::Wait { seconds } => wait(session, *seconds, limits).await,
        BrowserAction::ExtractDom {
            max_elements,
            filter,
            offset,
        } => extract_dom(session, limits, *max_elements, *offset, filter.as_deref()).await,
        BrowserAction::ReadPage {
            selector,
            max_chars,
        } => read_page(session, limits, selector.as_deref(), *max_chars).await,
        BrowserAction::PressKey {
            key,
            selector,
            agent_id,
        } => {
            let selector = target_selector(
                selector.as_deref(),
                *agent_id,
                snapshot,
                snapshot_slot,
                true,
            )
            .await?;
            press_key(session, *key, &selector, timeout).await
        }
        BrowserAction::Screenshot => screenshot(session, timeout).await,
        BrowserAction::ListTabs
        | BrowserAction::OpenTab { .. }
        | BrowserAction::SwitchTab { .. }
        | BrowserAction::CloseTab { .. } => {
            Err(tool_fail("the tab actions do not reach a page".to_owned()))
        }
    }
}

/// Reads the final page location and packs the answer.
pub(super) async fn finish_page(
    session: &CdpSession,
    digest: Digest,
    outcome: PageOutcome,
    limits: &BrowserLimits,
) -> Result<BrowserOutput> {
    let timeout: Duration = limits.command_timeout();
    let url = session.evaluate("window.location.href", timeout).await?;
    let title = session.evaluate("document.title", timeout).await?;
    Ok(BrowserOutput {
        url: url.as_str().unwrap_or_default().to_owned(),
        title: title.as_str().unwrap_or_default().to_owned(),
        message: outcome.message,
        elements: outcome.elements,
        elements_matched: outcome.elements_matched,
        elements_offset: outcome.elements_offset,
        dom_snapshot: outcome.dom_snapshot,
        page_text: outcome.page_text,
        page_text_origin: outcome.page_text_origin,
        screenshot_base64: outcome.screenshot,
        tabs: Vec::new(),
        request_digest: digest,
    })
}

/// Refreshes the element id token after a page action.
pub(super) async fn update_snapshot(slot: &Arc<Mutex<Option<String>>>, outcome: &PageOutcome) {
    *slot.lock().await = outcome.dom_snapshot.clone();
}
