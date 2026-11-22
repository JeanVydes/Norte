use std::sync::Arc;

use norte_core::{Error, Result};
use serde_json::Value;
use tokio::sync::Mutex;

use super::session::CdpSession;
use crate::browser::error::tool_fail;
use crate::browser::limits::BrowserLimits;

/// One page target the browser endpoint reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::browser) struct Tab {
    pub(in crate::browser) id: String,
    pub(in crate::browser) title: String,
    pub(in crate::browser) url: String,
    pub(in crate::browser) ws_url: String,
}

/// The page targets the endpoint has, with browser internals left out.
pub(in crate::browser) async fn list_targets(
    client: &reqwest::Client,
    limits: &BrowserLimits,
) -> Result<Vec<Tab>> {
    let url = format!("{}/json/list", limits.endpoint.trim_end_matches('/'));
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| tool_fail(format!("Cannot list browser tabs: {error}")))?;
    let targets: Vec<Value> = response
        .error_for_status()
        .map_err(|error| tool_fail(format!("Browser tab list failed: {error}")))?
        .json()
        .await
        .map_err(|error| tool_fail(format!("Invalid tabs list JSON from browser: {error}")))?;
    Ok(targets
        .iter()
        .filter(|target| {
            let tab_url = target["url"].as_str().unwrap_or_default();
            target["type"] == "page"
                && !tab_url.starts_with("chrome-extension://")
                && !tab_url.starts_with("devtools://")
                && target["webSocketDebuggerUrl"].as_str().is_some()
        })
        .map(|target| Tab {
            id: target["id"].as_str().unwrap_or_default().to_owned(),
            title: target["title"].as_str().unwrap_or_default().to_owned(),
            url: target["url"].as_str().unwrap_or_default().to_owned(),
            ws_url: target["webSocketDebuggerUrl"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
        .collect())
}

/// Resolves an id or a case-insensitive URL/title substring to one tab.
pub(in crate::browser) fn pick_tab<'a>(
    tabs: &'a [Tab],
    id: Option<&str>,
    needle: Option<&str>,
) -> Result<&'a Tab> {
    if let Some(id) = id {
        return tabs
            .iter()
            .find(|tab| tab.id == id)
            .ok_or_else(|| tool_fail(format!("no tab with id `{id}`; call list_tabs")));
    }
    let Some(needle) = needle else {
        return Err(Error::InvalidInput(
            "this action needs an id or a URL/title substring".to_owned(),
        ));
    };
    let needle = needle.to_ascii_lowercase();
    let matches: Vec<&Tab> = tabs
        .iter()
        .filter(|tab| {
            tab.url.to_ascii_lowercase().contains(&needle)
                || tab.title.to_ascii_lowercase().contains(&needle)
        })
        .collect();
    match matches.as_slice() {
        [only] => Ok(only),
        [] => Err(tool_fail(format!(
            "no open tab matches `{needle}`; call list_tabs to see them"
        ))),
        many => Err(tool_fail(format!(
            "`{needle}` matches {} tabs ({}); pass the id instead",
            many.len(),
            many.iter()
                .map(|tab| tab.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Encodes a URL as one query value for the browser's new-tab route.
pub(in crate::browser) fn encode_query(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~' | b':' | b'/') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0F)]));
        }
    }
    encoded
}

/// Opens a tab through the endpoint's own route.
pub(in crate::browser) async fn open_tab(
    client: &reqwest::Client,
    limits: &BrowserLimits,
    url: Option<&str>,
) -> Result<Tab> {
    let mut query = String::new();
    if let Some(url) = url {
        limits.validate_url(url)?;
        query.push('?');
        query.push_str(&encode_query(url));
    }
    let url = format!("{}/json/new{query}", limits.endpoint.trim_end_matches('/'));
    let response = client
        .put(url)
        .send()
        .await
        .map_err(|error| tool_fail(format!("Failed to open a tab: {error}")))?;
    let value: Value = response
        .error_for_status()
        .map_err(|error| tool_fail(format!("Opening a tab failed: {error}")))?
        .json()
        .await
        .map_err(|error| tool_fail(format!("Invalid new tab response: {error}")))?;
    Ok(Tab {
        id: value["id"].as_str().unwrap_or_default().to_owned(),
        title: value["title"].as_str().unwrap_or_default().to_owned(),
        url: value["url"].as_str().unwrap_or_default().to_owned(),
        ws_url: value["webSocketDebuggerUrl"]
            .as_str()
            .ok_or_else(|| tool_fail("No webSocketDebuggerUrl in the new tab".to_owned()))?
            .to_owned(),
    })
}

/// Closes a tab through the endpoint's own route.
pub(in crate::browser) async fn close_tab(
    client: &reqwest::Client,
    limits: &BrowserLimits,
    id: &str,
) -> Result<()> {
    let url = format!("{}/json/close/{id}", limits.endpoint.trim_end_matches('/'));
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| tool_fail(format!("Failed to close a tab: {error}")))?;
    response
        .error_for_status()
        .map_err(|error| tool_fail(format!("Closing a tab failed: {error}")))?;
    Ok(())
}

/// Returns the session for `tab`, reconnecting when the target or socket changed.
pub(in crate::browser) async fn connect_to(
    slot: &Arc<Mutex<Option<CdpSession>>>,
    limits: &BrowserLimits,
    tab: &Tab,
    bring_to_front: bool,
) -> Result<CdpSession> {
    let mut guard = slot.lock().await;
    let reconnect = guard.as_ref().is_none_or(|session| {
        session.endpoint() != limits.endpoint.trim_end_matches('/')
            || session.target_id() != tab.id
            || session.is_closed()
    });
    if reconnect {
        *guard = Some(CdpSession::connect(limits, tab, limits.command_timeout()).await?);
    }
    let session = guard
        .as_ref()
        .cloned()
        .ok_or_else(|| tool_fail("no CDP session is available".to_owned()))?;
    drop(guard);
    if bring_to_front {
        session
            .send(
                "Page.bringToFront",
                serde_json::json!({}),
                limits.command_timeout(),
            )
            .await?;
    }
    Ok(session)
}
