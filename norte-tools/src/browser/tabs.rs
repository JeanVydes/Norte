use std::sync::Arc;

use norte_core::{Digest, Result};
use tokio::sync::Mutex;

use super::cdp::{CdpSession, Tab, close_tab, connect_to, list_targets, open_tab, pick_tab};
use super::error::tool_fail;
use super::limits::BrowserLimits;
use super::output::BrowserTab;
use super::render::tab_output;
use super::request::BrowserRequest;

/// Runs tab-only actions and returns `None` for actions that need a page.
pub(super) async fn tab_action(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    client: &reqwest::Client,
    cdp_slot: &Arc<Mutex<Option<CdpSession>>>,
    current_slot: &Arc<Mutex<Option<String>>>,
    digest: &Digest,
) -> Result<Option<super::output::BrowserOutput>> {
    let current = current_slot.lock().await.clone();
    let output = match &request.action {
        super::request::BrowserAction::ListTabs => {
            let tabs = list_targets(client, limits).await?;
            let current = current
                .filter(|id| tabs.iter().any(|tab| tab.id == *id))
                .and_then(|id| {
                    tabs.iter()
                        .find(|tab| tab.id == id)
                        .map(|tab| (tab.url.as_str(), tab.title.as_str()))
                });
            tab_output(
                *digest,
                current,
                format!("{} tab(s) open", tabs.len()),
                report(&tabs, current_slot.lock().await.as_deref()),
            )
        }
        super::request::BrowserAction::OpenTab { url } => {
            let tab = open_tab(client, limits, url.as_deref()).await?;
            *current_slot.lock().await = Some(tab.id.clone());
            let tabs = list_targets(client, limits).await?;
            tab_output(
                *digest,
                Some((tab.url.as_str(), tab.title.as_str())),
                url.as_deref().map_or_else(
                    || "Opened a tab".to_owned(),
                    |url| format!("Opened a tab at {url}"),
                ),
                report(&tabs, Some(&tab.id)),
            )
        }
        super::request::BrowserAction::SwitchTab { id, url } => {
            let tabs = list_targets(client, limits).await?;
            let tab = pick_tab(&tabs, id.as_deref(), url.as_deref())?.clone();
            *current_slot.lock().await = Some(tab.id.clone());
            connect_to(cdp_slot, limits, &tab, true).await?;
            tab_output(
                *digest,
                Some((tab.url.as_str(), tab.title.as_str())),
                format!("Switched to {} ({})", tab.title, tab.url),
                report(&tabs, Some(&tab.id)),
            )
        }
        super::request::BrowserAction::CloseTab { id, url } => {
            close_one_tab(
                client,
                limits,
                cdp_slot,
                current_slot,
                id.as_deref(),
                url.as_deref(),
                digest,
            )
            .await?
        }
        _ => return Ok(None),
    };
    Ok(Some(output))
}

/// The tab the previous call left, or the first page the browser lists.
pub(super) async fn resolve_current_tab(
    client: &reqwest::Client,
    limits: &BrowserLimits,
    current_slot: &Arc<Mutex<Option<String>>>,
) -> Result<Tab> {
    let tabs = list_targets(client, limits).await?;
    let remembered = current_slot.lock().await.clone();
    let remembered_tab = remembered
        .as_deref()
        .and_then(|id| tabs.iter().find(|tab| tab.id == id))
        .cloned();
    if let Some(tab) = remembered_tab {
        return Ok(tab);
    }
    tabs.into_iter()
        .next()
        .ok_or_else(|| tool_fail("the browser has no page tab open".to_owned()))
}

async fn close_one_tab(
    client: &reqwest::Client,
    limits: &BrowserLimits,
    cdp_slot: &Arc<Mutex<Option<CdpSession>>>,
    current_slot: &Arc<Mutex<Option<String>>>,
    id: Option<&str>,
    needle: Option<&str>,
    digest: &Digest,
) -> Result<super::output::BrowserOutput> {
    let tabs = list_targets(client, limits).await?;
    if tabs.len() <= 1 {
        return Err(tool_fail(
            "refusing to close the last tab; the browser needs one page to drive".to_owned(),
        ));
    }
    let current = current_slot.lock().await.clone();
    let target = match (id, needle) {
        (None, None) => current
            .as_deref()
            .and_then(|id| tabs.iter().find(|tab| tab.id == id))
            .ok_or_else(|| tool_fail("no current tab to close; call list_tabs".to_owned()))?
            .clone(),
        _ => pick_tab(&tabs, id, needle)?.clone(),
    };
    close_tab(client, limits, &target.id).await?;
    if current.as_deref() == Some(target.id.as_str()) {
        *cdp_slot.lock().await = None;
    }
    let remaining = list_targets(client, limits).await?;
    let next = remaining.first().map(|tab| tab.id.clone());
    *current_slot.lock().await = next.clone();
    let page = next
        .as_deref()
        .and_then(|id| remaining.iter().find(|tab| tab.id == id))
        .map(|tab| (tab.url.as_str(), tab.title.as_str()));
    Ok(tab_output(
        *digest,
        page,
        format!("Closed the tab at {}", target.url),
        report(&remaining, next.as_deref()),
    ))
}

fn report(tabs: &[Tab], current: Option<&str>) -> Vec<BrowserTab> {
    tabs.iter()
        .map(|tab| BrowserTab {
            id: tab.id.clone(),
            title: tab.title.clone(),
            url: tab.url.clone(),
            current: current == Some(tab.id.as_str()),
        })
        .collect()
}
