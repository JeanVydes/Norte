use std::sync::Arc;
use std::time::Duration;

use norte_core::{Error, Result};
use serde_json::json;
use tokio::sync::Mutex;

use super::super::cdp::CdpSession;
use super::super::dom::{check_snapshot, key_event};
use super::super::error::tool_fail;
use super::super::outcome::PageOutcome;
use super::super::request::BrowserKey;

/// Resolves one target, keeping `selector` and `agent_id` mutually exclusive.
pub(in crate::browser) async fn target_selector(
    selector: Option<&str>,
    agent_id: Option<usize>,
    snapshot: Option<&str>,
    snapshot_slot: &Arc<Mutex<Option<String>>>,
    allow_focus: bool,
) -> Result<String> {
    match (selector, agent_id) {
        (Some(_), Some(_)) => Err(Error::InvalidInput(
            "pass either selector or agent_id, not both".to_owned(),
        )),
        (Some(selector), None) => Ok(selector.to_owned()),
        (None, Some(id)) => {
            check_snapshot(snapshot_slot, snapshot).await?;
            Ok(format!("[data-norte-agent-id='{id}']"))
        }
        (None, None) if allow_focus => Ok(":focus".to_owned()),
        (None, None) => Err(Error::InvalidInput(
            "this action needs a selector or an agent_id".to_owned(),
        )),
    }
}

pub(in crate::browser) async fn type_text(
    session: &CdpSession,
    selector: &str,
    text: &str,
    clear: bool,
    timeout: Duration,
) -> Result<PageOutcome> {
    let script = format!(
        r"
        (function() {{
            const el = document.querySelector({selector});
            if (!el) return false;
            el.focus();
            if ({clear}) el.value = '';
            el.value += {text};
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return true;
        }})();
        ",
        selector = serde_json::to_string(selector)?,
        clear = clear,
        text = serde_json::to_string(text)?
    );
    let typed = session.evaluate(&script, timeout).await?;
    if typed.as_bool() != Some(true) {
        return Err(tool_fail(format!(
            "Element not found to type into: {selector}"
        )));
    }
    Ok(PageOutcome {
        message: format!("Typed into {selector}"),
        ..PageOutcome::default()
    })
}

pub(in crate::browser) async fn scroll(
    session: &CdpSession,
    delta_x: f64,
    delta_y: f64,
    timeout: Duration,
) -> Result<PageOutcome> {
    let script = format!("window.scrollBy({delta_x}, {delta_y}); true;");
    session.evaluate(&script, timeout).await?;
    Ok(PageOutcome {
        message: format!("Scrolled window by ({delta_x}, {delta_y})"),
        ..PageOutcome::default()
    })
}

pub(in crate::browser) async fn press_key(
    session: &CdpSession,
    key: BrowserKey,
    selector: &str,
    timeout: Duration,
) -> Result<PageOutcome> {
    let focus = format!(
        "(function(){{const el=document.querySelector({selector});if(!el)return false;el.focus();return true;}})();",
        selector = serde_json::to_string(selector)?
    );
    let focused = session.evaluate(&focus, timeout).await?;
    if focused.as_bool() != Some(true) {
        return Err(tool_fail(format!("Element not found to focus: {selector}")));
    }
    let mut down = key_event(key);
    if matches!(key, BrowserKey::Enter | BrowserKey::Space) {
        down["text"] = json!(if key == BrowserKey::Enter { "\r" } else { " " });
    }
    down["type"] = json!("keyDown");
    let mut up = key_event(key);
    up["type"] = json!("keyUp");
    session
        .send("Input.dispatchKeyEvent", down, timeout)
        .await?;
    session.send("Input.dispatchKeyEvent", up, timeout).await?;
    Ok(PageOutcome {
        message: format!("Pressed {key:?}"),
        ..PageOutcome::default()
    })
}
