use std::sync::Arc;
use std::time::Duration;

use norte_core::{Digest, Result};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::cdp::CdpSession;
use super::error::{BrowserErrorCode, tool_fail, tool_fail_code};
use super::output::InteractiveElement;

/// The selector that defines a control from the page's point of view.
const CONTROL_SELECTOR: &str = "a[href], button, input, textarea, select, summary, \
     [role='button'], [role='link'], [role='checkbox'], [role='tab'], \
     [role='menuitem'], [role='switch'], [contenteditable='true'], [tabindex]";

/// A bounded view of the page's controls.
pub(super) struct DomSnapshot {
    /// The controls assigned ids in this extraction.
    pub(super) elements: Vec<InteractiveElement>,
    /// How many controls matched before offset and cap.
    pub(super) matched: usize,
    /// Token that must accompany later `agent_id` actions.
    pub(super) token: String,
}

#[derive(Deserialize)]
struct RawDom {
    elements: Vec<InteractiveElement>,
    matched: usize,
    fingerprint: String,
}

/// Maps visible controls to stable ids without putting their values in the text.
///
/// Filtering, paging and capping happen in the page, so a large page does not
/// cross CDP just to be discarded in Rust. Ids are assigned after filtering,
/// which keeps them stable for the returned page until the next extraction.
pub(super) async fn extract_interactive(
    session: &CdpSession,
    limit: usize,
    offset: usize,
    filter: Option<&str>,
    timeout_dur: Duration,
) -> Result<DomSnapshot> {
    let script = format!(
        r"
        (function() {{
            document.querySelectorAll('[data-norte-agent-id]')
                .forEach((el) => el.removeAttribute('data-norte-agent-id'));
            const needle = {needle}.toLowerCase();
            const controls = document.querySelectorAll({selector});
            const mapped = [];
            const fingerprint = [];
            let matched = 0;
            for (const el of controls) {{
                const style = window.getComputedStyle(el);
                const rect = el.getBoundingClientRect();
                if (rect.width === 0 || rect.height === 0 ||
                    style.visibility === 'hidden' || style.display === 'none') {{
                    continue;
                }}
                const type = (el.getAttribute('type') || el.type || '').toLowerCase();
                const autocomplete = (el.getAttribute('autocomplete') || '').toLowerCase();
                const sensitive = type === 'password' || autocomplete.includes('password') ||
                    autocomplete.includes('cc-');
                const label = Array.from(document.querySelectorAll('label'))
                    .find((candidate) => candidate.htmlFor === el.id);
                const labelText = label ? label.innerText : '';
                const aria = el.getAttribute('aria-label') || '';
                const name = el.getAttribute('name') || '';
                const href = el.tagName === 'A' ? el.getAttribute('href') : null;
                const text = sensitive
                    ? '[redacted]'
                    : String(el.innerText || el.value || el.placeholder || aria ||
                        labelText || el.title || name || el.id || '')
                        .trim().replace(/\s+/g, ' ').slice(0, 80);
                const fields = [text, el.placeholder, href, name, aria, labelText];
                if (needle && !fields.some((field) =>
                    String(field || '').toLowerCase().includes(needle))) {{
                    continue;
                }}
                matched += 1;
                if (fingerprint.length < 500) {{
                    fingerprint.push(el.tagName + ':' + text + ':' + (href || ''));
                }}
                if (matched <= {offset} || mapped.length >= {limit}) {{
                    continue;
                }}
                const id = mapped.length;
                el.setAttribute('data-norte-agent-id', String(id));
                mapped.push({{
                    id: id,
                    tag: el.tagName,
                    text: text,
                    input_type: el.getAttribute('type'),
                    placeholder: el.getAttribute('placeholder'),
                    href: href,
                    name: name || null,
                    aria_label: aria || null,
                    label: labelText || null,
                    role: el.getAttribute('role'),
                    in_viewport: rect.top < window.innerHeight && rect.bottom > 0 &&
                        rect.left < window.innerWidth && rect.right > 0,
                    disabled: el.disabled === true,
                    checked: ('checked' in el) ? el.checked === true : false
                }});
            }}
            return {{ elements: mapped, matched: matched, fingerprint: fingerprint.join('|') }};
        }})();
        ",
        needle = serde_json::to_string(filter.unwrap_or(""))?,
        selector = CONTROL_SELECTOR,
        offset = offset,
        limit = limit,
    );
    let value = session.evaluate(&script, timeout_dur).await?;
    let raw: RawDom = serde_json::from_value(value)
        .map_err(|error| tool_fail(format!("invalid DOM response: {error}")))?;
    let token = Digest::of(raw.fingerprint.as_bytes()).to_hex();
    Ok(DomSnapshot {
        elements: raw.elements,
        matched: raw.matched,
        token,
    })
}

/// Reads text from the page, or from one element when a selector is given.
pub(super) async fn read_page(
    session: &CdpSession,
    selector: Option<&str>,
    max_chars: usize,
    timeout_dur: Duration,
) -> Result<String> {
    let root = selector.unwrap_or("body");
    let script = format!(
        r"
        (function() {{
            const root = document.querySelector({root});
            if (!root) return null;
            return (root.innerText || '').trim().replace(/\n{{3,}}/g, '\n\n').slice(0, {max_chars});
        }})();
        ",
        root = serde_json::to_string(root)?,
        max_chars = max_chars,
    );
    let value = session.evaluate(&script, timeout_dur).await?;
    Ok(value.as_str().unwrap_or_default().to_owned())
}

/// Rejects an `agent_id` action whose snapshot does not match the last one.
pub(super) async fn check_snapshot(
    slot: &Arc<Mutex<Option<String>>>,
    provided: Option<&str>,
) -> Result<()> {
    let current = slot.lock().await.clone();
    let Some(provided) = provided else {
        return Err(tool_fail_code(
            "agent_id needs the snapshot token from the last extract_dom",
            BrowserErrorCode::ElementSnapshot,
        ));
    };
    match current {
        Some(current) if current == provided => Ok(()),
        Some(_) => Err(tool_fail_code(
            "the element ids changed; call extract_dom again and use its new snapshot",
            BrowserErrorCode::ElementSnapshot,
        )),
        None => Err(tool_fail_code(
            "call extract_dom before using an agent_id",
            BrowserErrorCode::ElementSnapshot,
        )),
    }
}

/// Parameters for a key event, as one CDP command can express them.
pub(super) fn key_event(key: super::request::BrowserKey) -> Value {
    let (name, code, virtual_key) = match key {
        super::request::BrowserKey::Enter => ("Enter", "Enter", 13),
        super::request::BrowserKey::Tab => ("Tab", "Tab", 9),
        super::request::BrowserKey::Escape => ("Escape", "Escape", 27),
        super::request::BrowserKey::ArrowUp => ("ArrowUp", "ArrowUp", 38),
        super::request::BrowserKey::ArrowDown => ("ArrowDown", "ArrowDown", 40),
        super::request::BrowserKey::ArrowLeft => ("ArrowLeft", "ArrowLeft", 37),
        super::request::BrowserKey::ArrowRight => ("ArrowRight", "ArrowRight", 39),
        super::request::BrowserKey::Backspace => ("Backspace", "Backspace", 8),
        super::request::BrowserKey::Delete => ("Delete", "Delete", 46),
        super::request::BrowserKey::Home => ("Home", "Home", 36),
        super::request::BrowserKey::End => ("End", "End", 35),
        super::request::BrowserKey::PageUp => ("PageUp", "PageUp", 33),
        super::request::BrowserKey::PageDown => ("PageDown", "PageDown", 34),
        super::request::BrowserKey::Space => (" ", "Space", 32),
    };
    json!({
        "key": name,
        "code": code,
        "windowsVirtualKeyCode": virtual_key,
        "nativeVirtualKeyCode": virtual_key,
    })
}
