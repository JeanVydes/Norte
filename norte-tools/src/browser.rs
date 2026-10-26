//! The browser automation executor for AI agents.
//!
//! Controls a real Chromium/`CDP`-enabled browser instance over the Chrome
//! `DevTools` protocol,
//! providing feedback-rich primitives (navigate, click, type, scroll, wait, extract/map DOM, screenshot).

use std::error::Error as StdError;
use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use norte_core::{
    Digest, Error, FnExecutor, Part, Result, ToolContext, ToolDeclaration, ToolMetadata, ToolName,
    ToolOutput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async_with_config};

/// Actions an agent can ask the browser to execute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BrowserAction {
    /// Navigate the active page to a specific URL.
    Navigate {
        /// Target URL (e.g. `https://example.com` or `https://github.com/login`).
        #[schemars(example = &"https://example.com")]
        url: String,
    },
    /// Click an element on the page (by `selector` or annotated numeric `agent_id`).
    Click {
        /// CSS selector of the element to click (e.g. `button.submit`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Numeric agent id from a prior `extract_dom` call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<usize>,
    },
    /// Type text into an element (or active focus).
    Type {
        /// Optional CSS selector (e.g. `input[name="email"]`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Optional numeric agent id injected during DOM extraction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<usize>,
        /// Text to input.
        text: String,
        /// When `true`, the field's current value is cleared before typing.
        /// When `false` (default), the text is appended to whatever is already
        /// there. Set to `true` to replace the whole field contents.
        #[serde(default)]
        #[schemars(
            default,
            description = "true clears the field before typing (replaces its contents); false appends to the existing value. Defaults to false."
        )]
        clear: bool,
    },
    /// Scroll the viewport.
    Scroll {
        /// Horizontal scroll delta in pixels (positive = right).
        #[serde(default)]
        delta_x: f64,
        /// Vertical scroll delta in pixels (positive = down).
        #[serde(default)]
        delta_y: f64,
    },
    /// Wait for a duration.
    Wait {
        /// Seconds to sleep (default 1.0, capped to the configured limit).
        #[serde(default = "default_wait_seconds")]
        seconds: f64,
    },
    /// Map and extract visible interactive elements (`a`, `button`, `input`,
    /// `textarea`, etc.) with assigned `agent_id`s.
    ///
    /// The bound and the filter live here, not beside `action`: only this action
    /// returns elements, and a field declared outside it is invisible in this
    /// variant's own schema — a model reading the `extract_dom` branch sees
    /// `{type}` and nothing else. They were once siblings of `action`, which had
    /// the second half of the problem too: a model that put them inside the
    /// action, where they belong, had them dropped without an error, because
    /// `deny_unknown_fields` does not apply to an internally tagged enum.
    ExtractDom {
        /// The most elements to return. `0` returns none, for a call that only
        /// needs the page; omitting it uses the host's ceiling. A larger number
        /// is clamped to that ceiling, and the answer reports what the cap left
        /// out.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            description = "The most elements to return. 0 returns none, for a call that only needs the page; omitting it uses the host's ceiling, and asking for more than that is clamped to it. The answer reports how many the cap left out."
        )]
        max_elements: Option<usize>,
        /// Keeps only the elements whose text, value or placeholder contains
        /// this, case-insensitively.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            description = "Keep only elements whose label contains this text (case-insensitive). Use it to pick a control out of a busy page instead of returning the whole list."
        )]
        filter: Option<String>,
    },
    /// Take a screenshot of the visible viewport as base64 PNG.
    Screenshot,
    /// List the tabs the browser already has open.
    ///
    /// What a person sees is a row of tabs, and the tool drove whichever one it
    /// found first. Listing them is what lets a call name the one it means
    /// instead of navigating the wrong one away.
    ListTabs,
    /// Move the tool to a tab the browser already has open.
    ///
    /// A different page is usually a tab that is already loaded, not a new one:
    /// switching keeps the person's own session, its cookies and its scroll
    /// position, which opening a tab does not.
    SwitchTab {
        /// The target id, as `ListTabs` reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// A substring of the tab's URL or title, when the id is not at hand.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Open a new tab, optionally at a URL.
    ///
    /// Spelled out as its own action so that opening a tab is a decision: a
    /// navigation that silently made one left the person looking at a window
    /// they did not ask for.
    OpenTab {
        /// The URL to open, or a blank tab when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Close a tab, by id or by a URL/title substring; the current one when
    /// neither is given.
    CloseTab {
        /// The target id, as `ListTabs` reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// A substring of the tab's URL or title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

fn default_wait_seconds() -> f64 {
    1.0
}

/// Request sent to the `browser_use` tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserRequest {
    /// Action to perform. See `BrowserAction` for the available variants.
    pub action: BrowserAction,
    /// Remote debugging endpoint (defaults to `http://127.0.0.1:9222`).
    /// Point at a running Chrome/Chromium with `--remote-debugging-port=9222`.
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
    /// Highlight the target element visually before interacting (default true).
    #[serde(default = "default_true")]
    pub highlight: bool,
}

fn default_endpoint() -> String {
    "http://127.0.0.1:9222".to_owned()
}

fn default_true() -> bool {
    true
}

/// Whether a flag is false, for a field that is left out when it is.
///
/// The reference is what `skip_serializing_if` hands a predicate, so the shape
/// is serde's and not this function's choice.
///
/// Left out unless true: the common case is a control that works, and a
/// `"disabled": false` on every row of a two-hundred-row list is the cost this
/// whole list is bounded to avoid.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// Whether a count is zero, for a field that is left out when it is.
///
/// The reference is what `skip_serializing_if` hands a predicate, so the shape
/// is serde's and not this function's choice.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// One tab the browser has open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTab {
    /// The target id, which `SwitchTab` and `CloseTab` accept.
    pub id: String,
    /// The tab's title.
    pub title: String,
    /// The tab's URL.
    pub url: String,
    /// Whether this is the tab the tool is driving.
    pub current: bool,
}

/// Representation of an interactive element on the page.
///
/// Every field past `tag` and `text` is optional and left out when it is absent
/// or false, so a plain button costs what it always did: a page with two hundred
/// controls is the case this list exists for, and a field that is usually absent
/// is a field that is usually paid for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveElement {
    /// Assigned integer ID for easy targeting by the LLM.
    pub id: usize,
    /// HTML tag name (`BUTTON`, `A`, `INPUT`, etc.).
    pub tag: String,
    /// Human-readable label or text.
    pub text: String,
    /// Input type (if tag is input).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// Placeholder attribute if present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Where an anchor goes, as the page wrote it — absolute or relative.
    ///
    /// Only for `A`. The text of a link says what it offers and not where it
    /// leads, so without this a model either guesses or clicks to find out, and
    /// a click that was a question navigates away from the page it was asking
    /// about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// Whether the control cannot be used.
    ///
    /// A disabled button looks like any other and does nothing, which is the
    /// failure a model cannot see: it calls `click`, the page does not change,
    /// and the reason is a field it was never shown.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// Whether a checkbox or radio is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub checked: bool,
}

/// Comprehensive feedback returned to the model after every browser action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserOutput {
    /// URL of the page after the action.
    pub url: String,
    /// Title of the page.
    pub title: String,
    /// Status message or summary of what was executed.
    pub message: String,
    /// Interactive DOM elements discovered on the page (populated on navigation or `ExtractDom`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<InteractiveElement>,
    /// How many elements matched before the limit cut the list.
    ///
    /// Present only when more matched than came back: a bounded list that does
    /// not admit it is bounded reads as the whole page, and a model that
    /// believes it has seen every control stops looking for the one it needs.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub elements_matched: usize,
    /// Base64-encoded PNG screenshot (if action was `Screenshot`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
    /// The browser's open tabs, from `ListTabs` and the actions that change
    /// which one is current. `current` marks the one the tool drives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<BrowserTab>,
    /// Digest of the request.
    pub request_digest: Digest,
}

/// Browser automation tool configuration limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BrowserLimits {
    /// Maximum wait seconds allowed per request.
    pub max_wait_seconds: u64,
    /// Timeout in seconds for individual CDP commands.
    pub cdp_timeout_seconds: u64,
    /// The most interactive elements one answer carries.
    ///
    /// A page's controls are what a model needs to act, but a page has hundreds
    /// of them and every one is a line of JSON in the context: past a screenful
    /// the list stops being read and starts being a bill. The answer says how
    /// many it left out, so a model that needs more can filter instead of
    /// guessing that it saw everything.
    ///
    /// A request may ask for fewer, and for none. It may not ask for more: this
    /// is a ceiling and not a default, because a bound the caller can raise is
    /// not a bound. A model that asked for `max_elements: 5000` on a page of
    /// 1200 links was handed 107 KB — a third of a context window — and the
    /// number it typed was not a decision anybody had made about that page.
    pub max_elements: usize,
}

impl Default for BrowserLimits {
    fn default() -> Self {
        Self {
            max_wait_seconds: 30,
            cdp_timeout_seconds: 15,
            max_elements: 80,
        }
    }
}

/// The browser automation tool.
#[derive(Clone)]
pub struct BrowserTool {
    limits: BrowserLimits,
    cdp_client: Arc<Mutex<Option<PersistentCdp>>>,
    /// The tab the tool is driving, if any. Kept beside the connection so a
    /// `navigate` reaches the tab the previous call used instead of whichever
    /// page the endpoint happens to list first.
    current_tab: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for BrowserTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserTool")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl BrowserTool {
    /// New tool instance with default limits.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: BrowserLimits::default(),
            cdp_client: Arc::new(Mutex::new(None)),
            current_tab: Arc::new(Mutex::new(None)),
        }
    }

    /// Sets custom limits.
    #[must_use]
    pub fn with_limits(mut self, limits: BrowserLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Turns the tool into a declaration the core registry accepts.
    #[must_use]
    pub fn into_declaration(self) -> ToolDeclaration<BrowserRequest> {
        ToolDeclaration::new(Self::NAME, Self::DESCRIPTION, Arc::new(self))
    }

    /// Executes the browser request.
    ///
    /// # Errors
    ///
    /// Returns an error when the action fails, when the CDP connection drops,
    /// or when the page reports a failure for the requested command.
    pub async fn run(&self, request: &BrowserRequest, _ctx: &ToolContext) -> Result<ToolOutput> {
        let outcome =
            execute_browser_action(request, &self.limits, &self.cdp_client, &self.current_tab)
                .await?;
        Ok(ToolOutput {
            parts: vec![Part::text(describe(&outcome))],
            structured: Some(serde_json::to_value(&outcome)?),
            is_error: false,
        })
    }
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolMetadata for BrowserTool {
    const NAME: &'static str = "browser_use";
    const DESCRIPTION: &'static str = "Controls a live browser via CDP (Chrome/Vivaldi/Brave/Edge). Actions: navigate (load a URL in the current tab), click (by CSS selector or agent_id from ExtractDom), type (text input; set clear=true to replace the field, false to append), scroll, wait, extract_dom (map interactive elements with agent_ids; each carries its tag, text and — when it has them — where a link goes, whether it is disabled, and whether a checkbox is checked; it takes max_elements to bound the list (0 for none; the host's ceiling applies whatever is asked), and filter to keep only the elements whose label, placeholder or link destination contains a text — both fields inside the action), screenshot (base64 PNG), list_tabs (the tabs already open, with the one being driven marked), switch_tab (drive a tab that is already open, by id or by a URL/title substring — prefer this to opening another), open_tab (open a new tab on purpose), close_tab. The tool drives one tab at a time and brings it to the front, so the person and the model look at the same page. Use ExtractDom first to get agent_ids for clicking/typing, and ListTabs before opening a tab: the page is often already open.";
}

#[async_trait::async_trait]
impl FnExecutor<BrowserRequest, ToolOutput> for BrowserTool {
    async fn call(&self, request: BrowserRequest) -> Result<ToolOutput> {
        self.run(&request, &ToolContext::new()).await
    }

    async fn call_with_context(
        &self,
        _ctx: &ToolContext,
        request: BrowserRequest,
    ) -> Result<ToolOutput> {
        self.run(&request, _ctx).await
    }
}

/// Build the browser tool as an erased function.
#[must_use]
pub fn browser_tool() -> Arc<dyn norte_core::AnyFunction> {
    Arc::new(BrowserTool::new().into_declaration())
}

// ---------------------------------------------------------------------------
// Internal Persistent CDP Client Implementation
// ---------------------------------------------------------------------------

fn tool_fail(reason: impl Into<String>) -> Error {
    let tool_name = ToolName::new(BrowserTool::NAME).expect("valid tool name");
    Error::ToolFailed {
        tool: tool_name,
        reason: reason.into(),
        code: None,
    }
}

struct PersistentCdp {
    endpoint: String,
    #[allow(dead_code)]
    target_id: String,
    ws: WebSocketStream<MaybeTlsStream<TcpStream>>,
    msg_id: i64,
}

impl PersistentCdp {
    async fn connect(
        endpoint: &str,
        target_id: &str,
        ws_url: &str,
    ) -> std::result::Result<Self, Box<dyn StdError + Send + Sync>> {
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(100 * 1024 * 1024); // 100 MB
        config.max_frame_size = Some(100 * 1024 * 1024); // 100 MB

        let (ws, _) = connect_async_with_config(ws_url, Some(config), false).await?;
        Ok(Self {
            endpoint: endpoint.to_string(),
            target_id: target_id.to_string(),
            ws,
            msg_id: 1,
        })
    }

    async fn send(
        &mut self,
        method: &str,
        params: Value,
        timeout_dur: Duration,
    ) -> std::result::Result<Value, Box<dyn StdError + Send + Sync>> {
        self.msg_id += 1;
        let id = self.msg_id;
        let req = json!({
            "id": id,
            "method": method,
            "params": params
        });

        self.ws.send(Message::Text(req.to_string().into())).await?;

        let read_future = async {
            while let Some(msg) = self.ws.next().await {
                let msg = msg?;
                if let Message::Text(text) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("id").and_then(Value::as_i64) == Some(id) {
                            if let Some(err) = v.get("error") {
                                return Err(format!("CDP Error in {method}: {err:?}").into());
                            }
                            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
                        }
                    }
                }
            }
            Err("CDP WebSocket stream closed unexpectedly".into())
        };

        match timeout(timeout_dur, read_future).await {
            Ok(res) => res,
            Err(_) => Err(format!("CDP command '{method}' timed out after {timeout_dur:?}").into()),
        }
    }

    async fn evaluate(
        &mut self,
        expr: &str,
        timeout_dur: Duration,
    ) -> std::result::Result<Value, Box<dyn StdError + Send + Sync>> {
        let res = self
            .send(
                "Runtime.evaluate",
                json!({
                    "expression": expr,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                timeout_dur,
            )
            .await?;
        Ok(res["result"]["value"].clone())
    }

    async fn wait_for_load_event(
        &mut self,
        timeout_dur: Duration,
    ) -> std::result::Result<(), Box<dyn StdError + Send + Sync>> {
        let wait_future = async {
            while let Some(msg) = self.ws.next().await {
                let msg = msg?;
                if let Message::Text(text) = msg {
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if v.get("method").and_then(Value::as_str) == Some("Page.loadEventFired") {
                            return Ok(());
                        }
                    }
                }
            }
            Err("CDP connection closed while waiting for Page.loadEventFired".into())
        };

        if let Ok(res) = timeout(timeout_dur, wait_future).await {
            return res;
        }
        // Si el evento ya ocurrió o la página fue rápida, verificar readyState
        if let Ok(ready) = self
            .evaluate("document.readyState", Duration::from_secs(2))
            .await
        {
            if ready.as_str() == Some("complete") || ready.as_str() == Some("interactive") {
                return Ok(());
            }
        }
        Err("Timed out waiting for Page.loadEventFired".into())
    }

    /// The page's visible interactive elements, as the page orders them.
    ///
    /// Everything visible is returned — the bound and the filter are the
    /// caller's, so one place decides what reaches the model.
    async fn extract_interactive(
        &mut self,
        timeout_dur: Duration,
    ) -> std::result::Result<Vec<InteractiveElement>, Box<dyn StdError + Send + Sync>> {
        let script = r#"
            (function() {
                let items = document.querySelectorAll('a, button, input, textarea, select, [role="button"]');
                let mapped = [];
                items.forEach((el, i) => {
                    let rect = el.getBoundingClientRect();
                    // Ignore elements that are not visible on screen
                    if (rect.width === 0 || rect.height === 0 || window.getComputedStyle(el).visibility === 'hidden') {
                        return;
                    }
                    el.setAttribute('data-agent-id', i.toString());
                    let text = el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || el.title || '';
                    // An anchor's destination, and only an anchor's: `href` on
                    // anything else is not what the model would mean by it. The
                    // page's own spelling is kept — relative stays relative — so
                    // what is reported is what the page says, not a guess at the
                    // absolute URL it resolves to.
                    let href = el.tagName === 'A' ? el.getAttribute('href') : null;
                    // What is checked is the property, not the attribute: the
                    // attribute is the page's first render and the property is
                    // what the control is now, after a click or a script.
                    let checked = ('checked' in el) ? el.checked : null;
                    mapped.push({
                        id: i,
                        tag: el.tagName,
                        // One line: a label with a newline in it is a block, and
                        // the newline buys nothing the model can use.
                        text: text.trim().replace(/\s+/g, ' ').slice(0, 80),
                        input_type: el.getAttribute('type'),
                        placeholder: el.getAttribute('placeholder'),
                        href: href,
                        disabled: el.disabled === true,
                        checked: checked === true
                    });
                });
                return JSON.stringify(mapped);
            })();
        "#;
        let val = self.evaluate(script, timeout_dur).await?;
        let json_str = val.as_str().unwrap_or("[]");
        let list: Vec<InteractiveElement> = serde_json::from_str(json_str).unwrap_or_default();
        Ok(list)
    }
}

/// What an action asked of its own element list.
///
/// Empty for every action but `extract_dom`, which is the only one that returns
/// elements. The two live in that variant's own schema so a model reading the
/// `extract_dom` branch sees them; as siblings of `action` they were invisible
/// there, and putting them inside the action had serde drop them silently.
#[derive(Default)]
struct Bound {
    max_elements: Option<usize>,
    filter: Option<String>,
}

/// The elements an answer carries: filtered, capped, and honest about the cap.
///
/// `elements` is what the page holds; `request` and `limits` decide what the
/// model gets. The filter is a case-insensitive substring of the label or the
/// placeholder, so one control can be asked for by name; the cap is the
/// request's, else the host's. Returns the kept elements and, when the cap hid
/// any, how many matched — a bounded list that does not say it is bounded reads
/// as the whole page.
fn bound_elements(
    elements: Vec<InteractiveElement>,
    max_elements: Option<usize>,
    filter: Option<&str>,
    limits: &BrowserLimits,
) -> (Vec<InteractiveElement>, usize) {
    let filtered: Vec<InteractiveElement> = match filter {
        Some(needle) if !needle.is_empty() => {
            let needle = needle.to_lowercase();
            elements
                .into_iter()
                .filter(|element| {
                    // The page's `value` is already folded into `text` by the
                    // extractor, so the label, the placeholder and the link's
                    // destination are the whole of what there is to match on. The
                    // href is here because finding a link by where it goes —
                    // "pricing" as a URL when the text says "See plans" — is the
                    // search a model actually has.
                    let haystack = format!(
                        "{} {} {}",
                        element.text,
                        element.placeholder.as_deref().unwrap_or_default(),
                        element.href.as_deref().unwrap_or_default()
                    )
                    .to_lowercase();
                    haystack.contains(&needle)
                })
                .collect()
        }
        _ => elements,
    };
    let matched = filtered.len();
    // `min`, not the request's value: the host's number is a ceiling and the
    // request can only ask for less. `None` is the host's own, which is what a
    // call that says nothing gets.
    let cap = max_elements
        .unwrap_or(limits.max_elements)
        .min(limits.max_elements);
    let mut kept = filtered;
    kept.truncate(cap);
    let hidden = matched.saturating_sub(kept.len());
    (kept, hidden)
}

/// One tab the endpoint reports, in the order the browser lists them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Tab {
    id: String,
    title: String,
    url: String,
    ws_url: String,
}

/// The tabs the endpoint has, with the extension pages and non-page targets out.
async fn list_targets(client: &reqwest::Client, endpoint: &str) -> Result<Vec<Tab>> {
    let tabs_url = format!("{}/json/list", endpoint.trim_end_matches('/'));
    let targets: Vec<Value> = client
        .get(&tabs_url)
        .send()
        .await
        .map_err(|e| {
            tool_fail(format!(
                "Cannot connect to browser endpoint {tabs_url}: {e}"
            ))
        })?
        .json()
        .await
        .map_err(|e| tool_fail(format!("Invalid tabs list JSON from browser: {e}")))?;
    Ok(targets
        .iter()
        .filter(|tab| {
            tab["type"] == "page"
                && !tab["url"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("chrome-extension://")
                && tab["webSocketDebuggerUrl"].as_str().is_some()
        })
        .map(|tab| Tab {
            id: tab["id"].as_str().unwrap_or_default().to_owned(),
            title: tab["title"].as_str().unwrap_or_default().to_owned(),
            url: tab["url"].as_str().unwrap_or_default().to_owned(),
            ws_url: tab["webSocketDebuggerUrl"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        })
        .collect())
}

/// The tab a name refers to: by id, else by a URL or title substring.
///
/// The id is exact and comes from [`list_targets`], so it is tried first. A
/// substring is what a person or a model has when they only saw the page: it
/// is matched against the URL and the title, and more than one is an error,
/// because driving the wrong tab is silent.
fn pick_tab<'a>(tabs: &'a [Tab], id: Option<&str>, url: Option<&str>) -> Result<&'a Tab> {
    if let Some(id) = id {
        return tabs
            .iter()
            .find(|tab| tab.id == id)
            .ok_or_else(|| tool_fail(format!("no tab with id `{id}`; call list_tabs")));
    }
    let Some(needle) = url else {
        return Err(Error::InvalidInput(
            "this action needs an id, a url substring, or nothing for the current tab".to_owned(),
        ));
    };
    let matches: Vec<&Tab> = tabs
        .iter()
        .filter(|tab| tab.url.contains(needle) || tab.title.contains(needle))
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

/// The tabs as a client reads them, with the driven one marked.
fn tab_report(tabs: &[Tab], current: Option<&str>) -> Vec<BrowserTab> {
    tabs.iter()
        .map(|tab| BrowserTab {
            id: tab.id.clone(),
            title: tab.title.clone(),
            url: tab.url.clone(),
            current: current == Some(tab.id.as_str()),
        })
        .collect()
}

/// Points the connection at `tab`, connecting if it is not already there.
///
/// The connection is to one tab, so a call that means a different one has to
/// reconnect: the alternative — one connection to the browser's tab target —
/// would need `Target.attachToTarget` sessions and a different command shape for
/// every call, which this tool does not carry.
async fn connect_to<'a>(
    endpoint: &str,
    tab: &Tab,
    cdp_slot: &'a Arc<Mutex<Option<PersistentCdp>>>,
    cmd_timeout: Duration,
) -> Result<tokio::sync::MutexGuard<'a, Option<PersistentCdp>>> {
    let mut guard = cdp_slot.lock().await;
    let already = guard
        .as_mut()
        .is_some_and(|client| client.endpoint == endpoint && client.target_id == tab.id);
    if !already {
        let mut persistent = PersistentCdp::connect(endpoint, &tab.id, &tab.ws_url)
            .await
            .map_err(|e| tool_fail(format!("CDP connection failed: {e}")))?;
        let _ = persistent.send("Page.enable", json!({}), cmd_timeout).await;
        let _ = persistent
            .send("Runtime.enable", json!({}), cmd_timeout)
            .await;
        *guard = Some(persistent);
    }
    // The person and the model look at the same page: the tab the tool drives is
    // brought to the front of the window, so a click the model makes is a click
    // the person can watch, and what the person does next is what the model
    // reads. Without this the tool drove a tab in the background.
    if let Some(client) = guard.as_mut() {
        let _ = client
            .send("Page.bringToFront", json!({}), cmd_timeout)
            .await;
    }
    Ok(guard)
}

/// The tab the tool is driving, remembering the one the last call left.
///
/// The first call of a process picks the first page the endpoint lists — the
/// frontmost tab of the window it opened with — and every call after it keeps
/// that tab, so a `navigate` does not jump to whichever page sorts first.
async fn current_tab(
    client: &reqwest::Client,
    endpoint: &str,
    cdp_slot: &Arc<Mutex<Option<PersistentCdp>>>,
    current_tab: &Arc<Mutex<Option<String>>>,
) -> Result<Tab> {
    let tabs = list_targets(client, endpoint).await?;
    // The connection knows the tab it is on even if nothing wrote it down.
    let remembered = current_tab.lock().await.clone().or_else(|| {
        cdp_slot
            .try_lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|client| client.target_id.clone()))
    });
    if let Some(id) = remembered
        && let Some(tab) = tabs.iter().find(|tab| tab.id == id)
    {
        return Ok(tab.clone());
    }
    tabs.into_iter()
        .next()
        .ok_or_else(|| tool_fail("the browser has no page tab open".to_owned()))
}

/// Opens a tab through the endpoint's own new-tab route.
async fn open_tab(client: &reqwest::Client, endpoint: &str, url: Option<&str>) -> Result<Tab> {
    let mut query = String::new();
    if let Some(url) = url {
        // The route takes the URL as its query string, so the caller's URL is
        // sent as one: encoding it keeps a `&` in the URL from being read as
        // another parameter.
        query = format!("?{url}");
    }
    let new_url = format!("{}/json/new{query}", endpoint.trim_end_matches('/'));
    let value: Value = client
        .put(&new_url)
        .send()
        .await
        .map_err(|e| tool_fail(format!("Failed to open a tab: {e}")))?
        .json()
        .await
        .map_err(|e| tool_fail(format!("Invalid new tab response: {e}")))?;
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
async fn close_tab(client: &reqwest::Client, endpoint: &str, id: &str) -> Result<()> {
    let url = format!("{}/json/close/{id}", endpoint.trim_end_matches('/'));
    let _ = client.get(&url).send().await;
    Ok(())
}

// One arm per browser action, each with its own script and readback; the length
// is the action count rather than a tangled control flow.
#[allow(clippy::too_many_lines)]
async fn execute_browser_action(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    cdp_slot: &Arc<Mutex<Option<PersistentCdp>>>,
    current_slot: &Arc<Mutex<Option<String>>>,
) -> Result<BrowserOutput> {
    let req_bytes = serde_json::to_vec(request)?;
    let request_digest = Digest::of(&req_bytes);
    let cmd_timeout = Duration::from_secs(limits.cdp_timeout_seconds);
    let endpoint = request.endpoint.trim_end_matches('/').to_owned();
    let client = reqwest::Client::new();

    // The tab actions are about the set of tabs, not about a page, so they are
    // served from the endpoint's own routes and never open a connection. Each
    // returns the browser's tabs so a model that just switched knows what it is
    // looking at and what else is there.
    match &request.action {
        BrowserAction::ListTabs => {
            let tabs = list_targets(&client, &endpoint).await?;
            let current = current_tab(&client, &endpoint, cdp_slot, current_slot)
                .await
                .ok()
                .map(|tab| tab.id);
            let reported = tab_report(&tabs, current.as_deref());
            let page = current
                .as_deref()
                .and_then(|id| tabs.iter().find(|tab| tab.id == id))
                .map(|tab| (tab.url.as_str(), tab.title.as_str()));
            return Ok(tab_output(
                request_digest,
                page,
                format!("{} tab(s) open", tabs.len()),
                reported,
            ));
        }
        BrowserAction::OpenTab { url } => {
            let tab = open_tab(&client, &endpoint, url.as_deref()).await?;
            *current_slot.lock().await = Some(tab.id.clone());
            let tabs = list_targets(&client, &endpoint).await?;
            return Ok(tab_output(
                request_digest,
                Some((tab.url.as_str(), tab.title.as_str())),
                match url {
                    Some(url) => format!("Opened a tab at {url}"),
                    None => "Opened a tab".to_owned(),
                },
                tab_report(&tabs, Some(&tab.id)),
            ));
        }
        BrowserAction::SwitchTab { id, url } => {
            let tabs = list_targets(&client, &endpoint).await?;
            let tab = pick_tab(&tabs, id.as_deref(), url.as_deref())?.clone();
            *current_slot.lock().await = Some(tab.id.clone());
            // Connecting is what brings it to the front, so a switch is visible
            // to the person at the same moment the model starts driving it.
            let _ = connect_to(&endpoint, &tab, cdp_slot, cmd_timeout).await;
            return Ok(tab_output(
                request_digest,
                Some((tab.url.as_str(), tab.title.as_str())),
                format!("Switched to {} ({})", tab.title, tab.url),
                tab_report(&tabs, Some(&tab.id)),
            ));
        }
        BrowserAction::CloseTab { id, url } => {
            let tabs = list_targets(&client, &endpoint).await?;
            let target = match pick_tab(&tabs, id.as_deref(), url.as_deref()) {
                Ok(tab) => tab.clone(),
                // With neither an id nor a substring the current tab is meant,
                // which is the one thing `pick_tab` cannot resolve on its own.
                Err(_) if id.is_none() && url.is_none() => {
                    current_tab(&client, &endpoint, cdp_slot, current_slot).await?
                }
                Err(error) => return Err(error),
            };
            close_tab(&client, &endpoint, &target.id).await?;
            let remaining = list_targets(&client, &endpoint).await?;
            // The closed tab cannot be driven again; move to whatever is left.
            let next = remaining.first().map(|tab| tab.id.clone());
            *current_slot.lock().await = next.clone();
            if let Some(next) = &next
                && let Some(tab) = remaining.iter().find(|tab| tab.id == *next)
            {
                let _ = connect_to(&endpoint, tab, cdp_slot, cmd_timeout).await;
            }
            let page = remaining
                .iter()
                .find(|tab| next.as_deref() == Some(tab.id.as_str()))
                .map(|tab| (tab.url.as_str(), tab.title.as_str()));
            return Ok(tab_output(
                request_digest,
                page,
                format!("Closed the tab at {}", target.url),
                tab_report(&remaining, next.as_deref()),
            ));
        }
        _ => {}
    }

    // Every other action runs against one tab: the one this call names if it
    // named one, else the one the last call left, else the first the browser
    // has. The tab is brought to the front, so the person watches the same page.
    let target = current_tab(&client, &endpoint, cdp_slot, current_slot).await?;
    *current_slot.lock().await = Some(target.id.clone());
    let mut guard = connect_to(&endpoint, &target, cdp_slot, cmd_timeout).await?;
    let cdp = guard
        .as_mut()
        .ok_or_else(|| tool_fail("Failed to obtain persistent CDP client"))?;

    // What `extract_dom` asked for, and nothing for the actions that return no
    // elements. It is not a field on the request because only one variant has
    // it: the schema a model reads is the schema the variant declares, and a
    // field declared outside it is invisible in that variant's branch.
    let mut bound = Bound::default();
    let (message, elements, screenshot_base64) = match &request.action {
        BrowserAction::Navigate { url } => {
            let _ = cdp
                .send("Page.navigate", json!({ "url": url }), cmd_timeout)
                .await;
            // Escuchar el evento Page.loadEventFired para prevenir condiciones de carrera
            let _ = cdp.wait_for_load_event(Duration::from_secs(10)).await;
            let els = cdp
                .extract_interactive(cmd_timeout)
                .await
                .unwrap_or_default();
            (format!("Navigated to {url}"), els, None)
        }
        BrowserAction::Click { selector, agent_id } => {
            let target_selector = match (selector, agent_id) {
                (Some(s), _) => s.clone(),
                (None, Some(id)) => format!("[data-agent-id='{id}']"),
                (None, None) => {
                    return Err(Error::InvalidInput(
                        "Click action requires selector or agent_id".to_owned(),
                    ));
                }
            };

            if request.highlight {
                let h_js = format!(
                    r"
                    (function() {{
                        let el = document.querySelector({});
                        if (el) {{
                            el.scrollIntoView({{ behavior: 'smooth', block: 'center' }});
                            el.style.outline = '4px solid #ff0055';
                            el.style.backgroundColor = 'rgba(255, 235, 59, 0.4)';
                        }}
                    }})();
                    ",
                    serde_json::to_string(&target_selector)?
                );
                let _ = cdp.evaluate(&h_js, cmd_timeout).await;
                sleep(Duration::from_millis(200)).await;
            }

            let click_js = format!(
                r"
                (function() {{
                    let el = document.querySelector({});
                    if (el) {{
                        el.click();
                        return true;
                    }}
                    return false;
                }})();
                ",
                serde_json::to_string(&target_selector)?
            );

            let res = cdp
                .evaluate(&click_js, cmd_timeout)
                .await
                .map_err(|e| tool_fail(e.to_string()))?;
            if res.as_bool() != Some(true) {
                return Err(tool_fail(format!("Element not found: {target_selector}")));
            }
            sleep(Duration::from_millis(400)).await;
            let els = cdp
                .extract_interactive(cmd_timeout)
                .await
                .unwrap_or_default();
            (
                format!("Clicked element matching {target_selector}"),
                els,
                None,
            )
        }
        BrowserAction::Type {
            selector,
            agent_id,
            text,
            clear,
        } => {
            let target_selector = match (selector, agent_id) {
                (Some(s), _) => s.clone(),
                (None, Some(id)) => format!("[data-agent-id='{id}']"),
                (None, None) => ":focus".to_string(),
            };

            let type_js = format!(
                r"
                (function() {{
                    let el = document.querySelector({});
                    if (!el) return false;
                    el.focus();
                    if ({}) {{
                        el.value = '';
                    }}
                    el.value += {};
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return true;
                }})();
                ",
                serde_json::to_string(&target_selector)?,
                clear,
                serde_json::to_string(text)?
            );

            let res = cdp
                .evaluate(&type_js, cmd_timeout)
                .await
                .map_err(|e| tool_fail(e.to_string()))?;
            if res.as_bool() != Some(true) {
                return Err(tool_fail(format!(
                    "Element not found to type into: {target_selector}"
                )));
            }
            (format!("Typed into {target_selector}"), Vec::new(), None)
        }
        BrowserAction::Scroll { delta_x, delta_y } => {
            let scroll_js = format!("window.scrollBy({delta_x}, {delta_y}); true;");
            let _ = cdp.evaluate(&scroll_js, cmd_timeout).await;
            (
                format!("Scrolled window by ({delta_x}, {delta_y})"),
                Vec::new(),
                None,
            )
        }
        BrowserAction::Wait { seconds } => {
            #[allow(clippy::cast_precision_loss)]
            let wait_secs = seconds.clamp(0.1, limits.max_wait_seconds as f64);
            sleep(Duration::from_secs_f64(wait_secs)).await;
            let els = cdp
                .extract_interactive(cmd_timeout)
                .await
                .unwrap_or_default();
            (format!("Waited for {wait_secs}s"), els, None)
        }
        BrowserAction::ExtractDom {
            max_elements,
            filter,
        } => {
            bound = Bound {
                max_elements: *max_elements,
                filter: filter.clone(),
            };
            let els = cdp
                .extract_interactive(cmd_timeout)
                .await
                .map_err(|e| tool_fail(e.to_string()))?;
            (
                format!("Extracted {} visible interactive elements", els.len()),
                els,
                None,
            )
        }
        BrowserAction::Screenshot => {
            let shot_res = cdp
                .send(
                    "Page.captureScreenshot",
                    json!({ "format": "png" }),
                    cmd_timeout,
                )
                .await
                .map_err(|e| tool_fail(format!("Screenshot capture failed: {e}")))?;
            if let Some(data) = shot_res["data"].as_str() {
                (
                    "Screenshot captured successfully".to_owned(),
                    Vec::new(),
                    Some(data.to_owned()),
                )
            } else {
                return Err(tool_fail("Failed to extract screenshot payload".to_owned()));
            }
        }
        // The tab actions returned before this match; the arm keeps the match
        // exhaustive without running a page command for one of them.
        BrowserAction::ListTabs
        | BrowserAction::OpenTab { .. }
        | BrowserAction::SwitchTab { .. }
        | BrowserAction::CloseTab { .. } => {
            return Err(tool_fail("the tab actions do not reach a page".to_owned()));
        }
    };

    // One place bounds the list, whatever action produced it: the filter first
    // — a page's controls are more than a model asked for, and naming one is
    // cheaper than receiving them all — and the cap second, with the count left
    // in the output so a cut list says it was cut. Only `extract_dom` sets the
    // two, so an action that produces no elements passes `None` for both.
    let (elements, elements_matched) = bound_elements(
        elements,
        bound.max_elements,
        bound.filter.as_deref(),
        limits,
    );

    let url_val = cdp
        .evaluate("window.location.href", cmd_timeout)
        .await
        .unwrap_or_default();
    let title_val = cdp
        .evaluate("document.title", cmd_timeout)
        .await
        .unwrap_or_default();

    Ok(BrowserOutput {
        url: url_val.as_str().unwrap_or("").to_string(),
        title: title_val.as_str().unwrap_or("").to_string(),
        message,
        elements_matched,
        elements,
        screenshot_base64,
        // The tab list belongs to the actions that change it. A page action
        // does not, and sending it every time is a round trip and a screenful
        // of JSON the model already has.
        tabs: Vec::new(),
        request_digest,
    })
}

/// The answer as text, which is what the model reads.
///
/// One line per element, `|`-separated, instead of the JSON object it is in
/// `structured`. The object repeats every key on every element, and the keys are
/// half the bytes: on a page of ordinary links, eighty elements are 7 096 bytes
/// of JSON against 4 450 of this — 37% less for the same fields, on the answer
/// that a real page makes the largest.
///
/// The fields are positional and fixed, so the header is written once:
///
/// ```text
/// id|tag|text|href|flags
/// 0|A|See plans|/pricing|
/// 3|BUTTON|Delete account||disabled
/// ```
///
/// `text` is not compressed, though it is the largest field: it is what the
/// model reads to decide, and a list of prefixes and suffixes is a list nobody
/// can act on. The bytes that are saved are the ones spent on punctuation.
fn describe(output: &BrowserOutput) -> String {
    use std::fmt::Write as _;
    let mut text = String::new();
    text.push_str("id|tag|text|href|flags\n");
    for element in &output.elements {
        let _ = write!(
            text,
            "{}|{}|{}|{}|",
            element.id,
            element.tag,
            // A `|` in a label would shift every column after it, so the field
            // that is the page's own text is the one that cannot contain the
            // separator.
            element.text.replace('|', "\\u{7c}"),
            element.href.as_deref().unwrap_or("")
        );
        let mut flags = Vec::new();
        if element.disabled {
            flags.push("disabled");
        }
        if element.checked {
            flags.push("checked");
        }
        let _ = write!(text, "{}", flags.join(","));
        text.push('\n');
    }
    text
}

/// The output of a tab action.
///
/// A pure tab action may end on a tab with no connection, so the page it
/// reports comes from what the endpoint listed and not from an evaluate that
/// would have to connect first. `page` is the tab the tool is on now, or `None`
/// when the action closed the last one.
fn tab_output(
    request_digest: Digest,
    page: Option<(&str, &str)>,
    message: String,
    tabs: Vec<BrowserTab>,
) -> BrowserOutput {
    let (url, title) = page.unwrap_or(("", ""));
    BrowserOutput {
        url: url.to_owned(),
        title: title.to_owned(),
        message,
        elements: Vec::new(),
        elements_matched: 0,
        screenshot_base64: None,
        tabs,
        request_digest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: &str, title: &str, url: &str) -> Tab {
        Tab {
            id: id.to_owned(),
            title: title.to_owned(),
            url: url.to_owned(),
            ws_url: format!("ws://127.0.0.1:9222/devtools/page/{id}"),
        }
    }

    /// Three tabs, the way a real window looks: an issue, a doc, a blank one.
    fn open_tabs() -> Vec<Tab> {
        vec![
            tab(
                "AAA",
                "Fix the flaky test",
                "https://github.com/o/r/issues/7",
            ),
            tab("BBB", "The Guide", "https://docs.example.com/guide"),
            tab("CCC", "New Tab", "about:blank"),
        ]
    }

    #[test]
    fn a_tab_is_picked_by_its_id_before_anything_else() {
        let tabs = open_tabs();
        let picked = pick_tab(&tabs, Some("BBB"), Some("github")).expect("the id wins");
        assert_eq!(picked.title, "The Guide");
    }

    #[test]
    fn a_tab_is_picked_by_a_url_or_title_substring() {
        let tabs = open_tabs();
        let by_url = pick_tab(&tabs, None, Some("docs.example")).expect("by url");
        assert_eq!(by_url.id, "BBB");
        // The title is searched too, so a model that saw the tab and not its
        // address can still name it.
        let by_title = pick_tab(&tabs, None, Some("flaky")).expect("by title");
        assert_eq!(by_title.id, "AAA");
    }

    #[test]
    fn a_substring_that_matches_two_tabs_is_refused_rather_than_guessed() {
        // Driving the wrong tab is silent, so two matches is an error naming
        // the ids: the caller can ask for the one it meant.
        let tabs = vec![
            tab("AAA", "One", "https://example.com/one"),
            tab("BBB", "Two", "https://example.com/two"),
        ];
        let error = pick_tab(&tabs, None, Some("example.com")).expect_err("ambiguous");
        let message = error.to_string();
        assert!(message.contains("AAA"), "{message}");
        assert!(message.contains("BBB"), "{message}");
    }

    #[test]
    fn a_name_that_matches_nothing_says_so_and_points_at_list_tabs() {
        let tabs = open_tabs();
        let error = pick_tab(&tabs, None, Some("nowhere")).expect_err("no match");
        assert!(error.to_string().contains("list_tabs"), "{error}");
    }

    fn element(id: usize, tag: &str, text: &str) -> InteractiveElement {
        InteractiveElement {
            id,
            tag: tag.to_owned(),
            text: text.to_owned(),
            input_type: None,
            placeholder: None,
            href: None,
            disabled: false,
            checked: false,
        }
    }

    #[test]
    fn the_element_list_is_capped_at_the_host_limit_by_default() {
        // A page has hundreds of controls and every one is context. The default
        // cap is the host's, and what it hid is reported so the list does not
        // read as the whole page.
        let limits = BrowserLimits {
            max_elements: 3,
            ..BrowserLimits::default()
        };
        let page: Vec<_> = (0..10)
            .map(|index| element(index, "A", &format!("link {index}")))
            .collect();
        let (kept, hidden) = bound_elements(page, None, None, &limits);
        assert_eq!(kept.len(), 3);
        assert_eq!(hidden, 7, "the seven left out are counted");
    }

    /// The bound, against a browser that is actually running.
    ///
    /// `#[ignore]`d because it needs a live CDP endpoint with a page full of
    /// controls, which is not something a unit test can arrange. It is the only
    /// test that proves the fields reach the executor over the real path: the
    /// others pin the schema and the parse, and this one pins the answer.
    ///
    /// ```text
    /// vivaldi --remote-debugging-port=9222 --user-data-dir=/tmp/vivaldi-debug \
    ///         <file:///tmp/vivaldi-debug/page.html>
    /// cargo test -p norte-tools -- --ignored the_bound_reaches_a_live_browser
    /// ```
    #[tokio::test]
    #[ignore = "needs a browser with --remote-debugging-port=9222"]
    async fn the_bound_reaches_a_live_browser() {
        // The whole page, capped only by the host's limit, which is high enough
        // for a test page. Read as `InteractiveElement`s rather than as JSON so
        // the assertions below are about the fields, not about a map.
        async fn full_elements() -> Vec<InteractiveElement> {
            let tool = BrowserTool::new();
            let request: BrowserRequest =
                serde_json::from_value(json!({"action": {"type": "extract_dom"}})).expect("parses");
            let out = tool
                .call_with_context(&ToolContext::new(), request)
                .await
                .expect("runs");
            let structured = out.structured.expect("structured");
            serde_json::from_value(structured["elements"].clone()).expect("the elements parse")
        }

        let tool = BrowserTool::new();
        let ask = |value: serde_json::Value| async {
            let request: BrowserRequest = serde_json::from_value(value).expect("parses");
            let out = tool
                .call_with_context(&ToolContext::new(), request)
                .await
                .expect("the call runs");
            let structured = out.structured.expect("structured output");
            let returned = structured["elements"].as_array().map_or(0, Vec::len);
            let matched = structured["elements_matched"].as_u64().unwrap_or(0);
            (returned, matched)
        };

        // The page the controls are on, put there by the test itself so it does
        // not depend on what the browser happened to have open. It is a local
        // file: the point is the bound, not the network.
        let page = "/tmp/vivaldi-debug/page.html";
        if !std::path::Path::new(page).exists() {
            eprintln!("skipping: {page} is not there");
            return;
        }
        let _ = tool
            .call_with_context(
                &ToolContext::new(),
                serde_json::from_value(json!({
                    "action": {"type": "navigate", "url": format!("file://{page}")}
                }))
                .expect("navigate"),
            )
            .await;

        let (all, _matched) = ask(json!({"action": {"type": "extract_dom"}})).await;
        assert!(all > 4, "the page has controls to bound: {all}");

        // The page the test navigates to has the three things a control can
        // carry besides its label: a link's destination, a disabled button and a
        // checked box. This is the only place they are read from a real DOM.
        let elements = full_elements().await;
        let link = elements
            .iter()
            .find(|element| element.tag == "A" && element.text == "See plans")
            .expect("the pricing link");
        assert_eq!(
            link.href.as_deref(),
            Some("/pricing"),
            "the link says where"
        );
        assert!(
            elements
                .iter()
                .any(|element| element.tag == "BUTTON" && element.disabled),
            "the disabled button is marked: {elements:?}"
        );
        assert!(
            elements
                .iter()
                .any(|element| element.tag == "INPUT" && element.checked),
            "the checked box is marked"
        );
        // And a plain button carries none of it, which is what keeps a long list
        // from paying for fields that are usually absent.
        let plain = elements
            .iter()
            .find(|element| element.tag == "BUTTON" && element.text == "Sign in")
            .expect("the plain button");
        assert!(plain.href.is_none() && !plain.disabled && !plain.checked);

        let (kept, _) = ask(json!({"action": {"type": "extract_dom", "max_elements": 3}})).await;
        assert_eq!(kept, 3, "the bound arrived: {kept} of {all}");

        // `elements_matched` counts what the cap left OUT, not what came back —
        // it is present only when something was hidden, so a whole list carries
        // no count at all.
        let (filtered, hidden) =
            ask(json!({"action": {"type": "extract_dom", "filter": "sign"}})).await;
        assert!(filtered < all, "the filter arrived: {filtered} of {all}");
        assert_eq!(hidden, 0, "nothing was hidden by a filter alone");

        let (one, hidden) = ask(json!({
            "action": {"type": "extract_dom", "filter": "sign", "max_elements": 1}
        }))
        .await;
        assert_eq!(one, 1, "the two compose");
        assert_eq!(
            hidden,
            u64::try_from(filtered - 1).expect("fits"),
            "and the count says how many the cap hid"
        );
    }

    #[test]
    fn the_schema_declares_the_bound_where_a_model_reads_it() {
        // The bug this pins: `max_elements` and `filter` were siblings of
        // `action`, so the `extract_dom` branch of the schema declared `{type}`
        // and nothing else — a model reading that branch had no way to know the
        // fields existed. Moving them into `action` instead was worse: serde
        // drops an unknown field of an internally tagged enum without an error,
        // so the call looked accepted and the bound was silently the host's.
        let declaration = BrowserTool::new().into_declaration();
        let schema = serde_json::to_value(&declaration.parameters).expect("schema");

        // Not at the top level: that is where they were invisible.
        let top = schema["properties"].as_object().expect("properties");
        assert!(
            !top.contains_key("max_elements"),
            "the bound is not a sibling of `action`: {schema}"
        );

        let variants = schema["$defs"]["BrowserAction"]["oneOf"]
            .as_array()
            .expect("variants");
        let extract = variants
            .iter()
            .find(|variant| variant["properties"]["type"]["const"].as_str() == Some("extract_dom"))
            .expect("the extract_dom branch");
        let props = extract["properties"].as_object().expect("its properties");
        assert!(
            props.contains_key("max_elements"),
            "the branch a model reads declares the bound: {extract}"
        );
        assert!(props.contains_key("filter"), "and the filter: {extract}");
    }

    #[test]
    fn a_bound_inside_the_action_reaches_the_tool() {
        // The shape a model writes, and the one that used to be dropped in
        // silence: the fields inside the action, not beside it.
        let request: BrowserRequest = serde_json::from_value(serde_json::json!({
            "action": {"type": "extract_dom", "max_elements": 3, "filter": "sign"}
        }))
        .expect("a call with the bound inside the action parses");
        let BrowserAction::ExtractDom {
            max_elements,
            filter,
        } = request.action
        else {
            panic!("it is an extract_dom");
        };
        assert_eq!(max_elements, Some(3), "the bound arrived");
        assert_eq!(filter.as_deref(), Some("sign"), "and the filter");
    }

    #[test]
    fn a_request_can_ask_for_fewer_or_for_none() {
        let limits = BrowserLimits::default();
        let page: Vec<_> = (0..10)
            .map(|index| element(index, "A", &format!("link {index}")))
            .collect();
        let (kept, hidden) = bound_elements(page.clone(), Some(2), None, &limits);
        assert_eq!(kept.len(), 2);
        assert_eq!(hidden, 8);

        // Zero is how a call that only wants the page's URL and title avoids
        // paying for the DOM at all. It is not "no limit": that is the default.
        let (none, hidden) = bound_elements(page, Some(0), None, &limits);
        assert!(none.is_empty());
        assert_eq!(hidden, 10);
    }

    #[test]
    fn a_filter_keeps_only_the_named_control() {
        // Picking one control out of a busy page is the cheap path: the list
        // never comes back to be searched.
        let limits = BrowserLimits::default();
        let page = vec![
            element(0, "A", "Sign in"),
            element(1, "A", "Documentation"),
            element(2, "BUTTON", "Sign out"),
        ];
        let (kept, hidden) = bound_elements(page, None, Some("sign"), &limits);
        assert_eq!(kept.len(), 2, "case does not matter: {kept:?}");
        assert_eq!(kept[0].text, "Sign in");
        assert_eq!(kept[1].text, "Sign out");
        assert_eq!(hidden, 0, "the filter matched fewer than the cap");
    }

    #[test]
    fn the_text_answer_is_a_table_and_keeps_every_field() {
        // What the model reads, as opposed to `structured`, which is what the
        // host keeps. The object repeats its keys on every element and the keys
        // are half the bytes; one line per element with fixed columns is the
        // same fields for 40% less on a page of ordinary links.
        let mut link = element(5, "A", "See plans");
        link.href = Some("/pricing".to_owned());
        let mut off = element(3, "BUTTON", "Delete account");
        off.disabled = true;
        let mut box_ = element(9, "INPUT", "");
        box_.checked = true;
        box_.placeholder = Some("Email".to_owned());

        let output = BrowserOutput {
            url: "https://example.test/".to_owned(),
            title: "Example".to_owned(),
            message: "Extracted 3 visible interactive elements".to_owned(),
            elements: vec![link, off, box_],
            elements_matched: 0,
            screenshot_base64: None,
            tabs: Vec::new(),
            request_digest: Digest::of(b"request"),
        };
        let text = describe(&output);

        assert_eq!(
            text,
            "id|tag|text|href|flags\n\
             5|A|See plans|/pricing|\n\
             3|BUTTON|Delete account||disabled\n\
             9|INPUT|||checked\n",
            "one header and one line per element"
        );
        // The flags are the only field that varies in position, so it is the
        // last one: a row with none ends in an empty field, not a short row.
        for line in text.lines().skip(1) {
            assert_eq!(
                line.matches('|').count(),
                4,
                "every row has the same columns: {line}"
            );
        }
    }

    #[test]
    fn a_label_with_the_separator_in_it_does_not_shift_the_columns() {
        // The page's own text is the one field this tool does not write, so it
        // is the one that can contain the separator. A label with a `|` in it
        // would move every column after it, and a model reading the row would
        // take the second half of a label for the link's destination.
        let mut odd = element(0, "A", "One | Two | Three");
        odd.href = Some("/a".to_owned());
        let output = BrowserOutput {
            url: String::new(),
            title: String::new(),
            message: String::new(),
            elements: vec![odd],
            elements_matched: 0,
            screenshot_base64: None,
            tabs: Vec::new(),
            request_digest: Digest::of(b"request"),
        };
        let text = describe(&output);
        let row = text.lines().nth(1).expect("one row");
        assert_eq!(
            row.matches('|').count(),
            4,
            "the label did not add columns: {row}"
        );
        assert!(row.contains("One \\u{7c} Two \\u{7c} Three"), "{row}");
    }

    #[test]
    fn the_host_limit_is_a_ceiling_a_request_cannot_raise() {
        // A bound the caller can raise is not a bound. A model that asked for
        // `max_elements: 5000` on a page of 1200 links was handed 107 KB — a
        // third of a context window — for a number it typed without knowing how
        // big the page was.
        let limits = BrowserLimits {
            max_elements: 80,
            ..BrowserLimits::default()
        };
        let page: Vec<_> = (0..1200)
            .map(|index| element(index, "A", &format!("Article {index}")))
            .collect();

        // Asking for more gets the ceiling, and the count says so.
        let (kept, hidden) = bound_elements(page.clone(), Some(5000), None, &limits);
        assert_eq!(kept.len(), 80, "the ceiling holds");
        assert_eq!(hidden, 1120, "and the answer admits what it left out");

        // Asking for nothing gets the ceiling, which is the host's default.
        let (kept, _) = bound_elements(page.clone(), None, None, &limits);
        assert_eq!(kept.len(), 80);

        // Asking for less gets less: the request decides downward.
        let (kept, hidden) = bound_elements(page.clone(), Some(3), None, &limits);
        assert_eq!(kept.len(), 3);
        assert_eq!(hidden, 1197);

        // And for none gets none, which is the one thing it may do below the cap.
        let (kept, hidden) = bound_elements(page, Some(0), None, &limits);
        assert!(kept.is_empty());
        assert_eq!(hidden, 1200);
    }

    #[test]
    fn a_link_reports_where_it_goes() {
        // The text of a link says what it offers, not where it leads. Without
        // the href a model either guesses or clicks to find out, and a click
        // that was a question navigates away from the page it was asking about.
        let mut link = element(0, "A", "See plans");
        link.href = Some("/pricing".to_owned());
        let page = vec![link, element(1, "BUTTON", "Sign in")];

        let (kept, _) = bound_elements(page.clone(), None, Some("pricing"), &limits());
        assert_eq!(kept.len(), 1, "the link is found by where it goes");
        assert_eq!(kept[0].id, 0);

        // And only an anchor carries one: a button with an href attribute is
        // not a thing the extractor reports.
        let (all, _) = bound_elements(page, None, None, &limits());
        assert_eq!(all[0].href.as_deref(), Some("/pricing"));
        assert!(all[1].href.is_none(), "a button has nowhere to go");
    }

    #[test]
    fn a_plain_element_costs_what_it_always_did() {
        // The list exists because a page has hundreds of controls, so a field
        // that is usually absent must not be written on every row. `disabled:
        // false` on two hundred elements is context spent saying nothing.
        let plain = serde_json::to_value(element(0, "BUTTON", "Sign in")).expect("json");
        let object = plain.as_object().expect("object");
        assert!(!object.contains_key("href"), "{plain}");
        assert!(!object.contains_key("disabled"), "{plain}");
        assert!(!object.contains_key("checked"), "{plain}");

        // And each appears when it is the thing a model needs to know.
        let mut off = element(1, "BUTTON", "Delete");
        off.disabled = true;
        let json = serde_json::to_value(&off).expect("json");
        assert_eq!(json["disabled"], serde_json::json!(true), "{json}");

        let mut box_ = element(2, "INPUT", "");
        box_.checked = true;
        let json = serde_json::to_value(&box_).expect("json");
        assert_eq!(json["checked"], serde_json::json!(true), "{json}");
    }

    fn limits() -> BrowserLimits {
        BrowserLimits::default()
    }

    #[test]
    fn the_filter_looks_at_the_placeholder_too() {
        // A search box's label is often only a placeholder, which is the one
        // thing that names it.
        let limits = BrowserLimits::default();
        let mut box_ = element(0, "INPUT", "");
        box_.placeholder = Some("Search the docs".to_owned());
        let page = vec![box_, element(1, "A", "Home")];
        let (kept, _) = bound_elements(page, None, Some("docs"), &limits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, 0);
    }

    #[test]
    fn a_bounded_list_that_hid_nothing_reports_nothing() {
        // The count is present only when it means something: a page smaller
        // than the cap is complete, and saying "0 hidden" is noise in the
        // output the model reads on every action.
        let limits = BrowserLimits::default();
        let page = vec![element(0, "A", "Home")];
        let (kept, hidden) = bound_elements(page, None, None, &limits);
        assert_eq!(kept.len(), 1);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn the_report_marks_the_tab_the_tool_drives() {
        let tabs = open_tabs();
        let report = tab_report(&tabs, Some("BBB"));
        assert_eq!(report.len(), 3);
        assert!(!report[0].current);
        assert!(report[1].current, "the driven tab is the one marked");
        assert!(!report[2].current);
        // With no current tab — one was just closed, or none is open — every
        // entry is unmarked rather than the first one claiming to be it.
        let none = tab_report(&tabs, None);
        assert!(none.iter().all(|tab| !tab.current));
    }
}
