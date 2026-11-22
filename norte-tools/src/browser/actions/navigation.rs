use std::collections::BTreeSet;
use std::time::Duration;

use norte_core::Result;
use serde_json::{Value, json};
use tokio::time::sleep;

use super::super::cdp::{self, CdpSession, wait_for_event};
use super::super::error::{BrowserErrorCode, tool_fail, tool_fail_code};
use super::super::limits::BrowserLimits;
use super::super::outcome::PageOutcome;
use super::current_elements;

const AFTER_CLICK: Duration = Duration::from_millis(400);
const HIGHLIGHT_PAUSE: Duration = Duration::from_millis(200);

pub(in crate::browser) async fn navigate(
    session: &CdpSession,
    limits: &BrowserLimits,
    url: &str,
) -> Result<PageOutcome> {
    limits.validate_url(url)?;
    let mut events = session.subscribe();
    let response = session
        .send(
            "Page.navigate",
            json!({ "url": url }),
            limits.command_timeout(),
        )
        .await?;
    if let Some(error) = response["errorText"].as_str() {
        return Err(tool_fail_code(
            format!("navigation to {url} failed: {error}"),
            BrowserErrorCode::Navigation,
        ));
    }
    let mut load_note = "";
    if let Some(loader_id) = response["loaderId"].as_str() {
        let loader = loader_id.to_owned();
        let load_result = wait_for_event(
            &mut events,
            "Page.lifecycleEvent",
            |params: &Value| {
                params["loaderId"].as_str() == Some(loader.as_str())
                    && params["name"].as_str() == Some("load")
            },
            limits.load_timeout(),
        )
        .await;
        if load_result.is_err() {
            load_note = " (load event was not observed before the host timeout)";
        }
    }
    Ok(PageOutcome {
        message: format!("Navigated to {url}{load_note}"),
        elements: current_elements(session, limits).await,
        ..PageOutcome::default()
    })
}

pub(in crate::browser) async fn click(
    session: &CdpSession,
    client: &reqwest::Client,
    limits: &BrowserLimits,
    selector: &str,
    highlight: bool,
) -> Result<PageOutcome> {
    let before = cdp::list_targets(client, limits)
        .await
        .map(|tabs| tabs.into_iter().map(|tab| tab.id).collect::<BTreeSet<_>>())
        .unwrap_or_default();
    let quoted = serde_json::to_string(selector)?;
    if highlight {
        let script = format!(
            "(function(){{const el=document.querySelector({quoted});if(el){{el.scrollIntoView({{block:'center'}});el.style.outline='4px solid #ff0055';}}}})();"
        );
        let _ = session.evaluate(&script, limits.command_timeout()).await;
        sleep(HIGHLIGHT_PAUSE).await;
    }
    let script = format!(
        "(function(){{const el=document.querySelector({quoted});if(!el)return false;el.click();return true;}})();"
    );
    let clicked = session.evaluate(&script, limits.command_timeout()).await?;
    if clicked.as_bool() != Some(true) {
        return Err(tool_fail(format!("Element not found: {selector}")));
    }
    sleep(AFTER_CLICK).await;
    let opened = cdp::list_targets(client, limits)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|tab| !before.contains(&tab.id))
        .map(|tab| tab.id)
        .collect::<Vec<_>>();
    let popup_note = if opened.is_empty() {
        String::new()
    } else {
        format!(" A new tab opened: {}.", opened.join(", "))
    };
    Ok(PageOutcome {
        message: format!("Clicked element matching {selector}.{popup_note}"),
        elements: current_elements(session, limits).await,
        ..PageOutcome::default()
    })
}

pub(in crate::browser) async fn wait(
    session: &CdpSession,
    seconds: f64,
    limits: &BrowserLimits,
) -> Result<PageOutcome> {
    #[allow(clippy::cast_precision_loss)]
    let maximum = limits.max_wait_seconds.max(1) as f64;
    let wait_seconds = if seconds.is_finite() {
        seconds.clamp(0.1, maximum)
    } else {
        0.1
    };
    sleep(Duration::from_secs_f64(wait_seconds)).await;
    Ok(PageOutcome {
        message: format!("Waited for {wait_seconds}s"),
        elements: current_elements(session, limits).await,
        ..PageOutcome::default()
    })
}
