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
    Digest, Error, FnExecutor, Result, ToolContext, ToolDeclaration, ToolMetadata, ToolName,
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
    /// Map and extract visible interactive elements (`a`, `button`, `input`, `textarea`, etc.) with assigned `agent_id`s.
    ExtractDom,
    /// Take a screenshot of the visible viewport as base64 PNG.
    Screenshot,
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

/// Representation of an interactive element on the page.
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
    /// Base64-encoded PNG screenshot (if action was `Screenshot`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
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
}

impl Default for BrowserLimits {
    fn default() -> Self {
        Self {
            max_wait_seconds: 30,
            cdp_timeout_seconds: 15,
        }
    }
}

/// The browser automation tool.
#[derive(Clone)]
pub struct BrowserTool {
    limits: BrowserLimits,
    cdp_client: Arc<Mutex<Option<PersistentCdp>>>,
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
        let outcome = execute_browser_action(request, &self.limits, &self.cdp_client).await?;
        let json_val = serde_json::to_value(&outcome)?;
        Ok(ToolOutput::json(json_val))
    }
}

impl Default for BrowserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolMetadata for BrowserTool {
    const NAME: &'static str = "browser_use";
    const DESCRIPTION: &'static str = "Controls a live browser via CDP (Chrome/Vivaldi/Brave/Edge). Actions: navigate (load a URL), click (by CSS selector or agent_id from ExtractDom), type (text input; set clear=true to replace the field, false to append), scroll, wait, extract_dom (map interactive elements with agent_ids), screenshot (base64 PNG). Use ExtractDom first to get agent_ids for clicking/typing.";
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
                    let text = el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || '';
                    mapped.push({
                        id: i,
                        tag: el.tagName,
                        text: text.trim().slice(0, 100),
                        input_type: el.getAttribute('type'),
                        placeholder: el.getAttribute('placeholder')
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

async fn get_or_create_cdp<'a>(
    endpoint: &str,
    cdp_slot: &'a Arc<Mutex<Option<PersistentCdp>>>,
    cmd_timeout: Duration,
) -> Result<tokio::sync::MutexGuard<'a, Option<PersistentCdp>>> {
    let mut guard = cdp_slot.lock().await;

    // Verificar si la conexión existente sigue viva
    if let Some(client) = guard.as_mut()
        && client.endpoint == endpoint
        && client
            .evaluate("1 + 1", Duration::from_millis(500))
            .await
            .is_ok()
    {
        return Ok(guard);
    }

    // Si no existe o se desconectó, reconectar
    let client_http = reqwest::Client::new();
    let tabs_url = format!("{}/json/list", endpoint.trim_end_matches('/'));
    let tabs: Vec<Value> = client_http
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

    let target_tab = tabs.iter().find(|t| {
        t["type"] == "page"
            && !t["url"]
                .as_str()
                .unwrap_or("")
                .starts_with("chrome-extension://")
            && t["webSocketDebuggerUrl"].as_str().is_some()
    });

    let (tab_id, ws_url) = if let Some(tab) = target_tab {
        (
            tab["id"].as_str().unwrap_or("").to_string(),
            tab["webSocketDebuggerUrl"].as_str().unwrap().to_string(),
        )
    } else {
        let new_url = format!("{}/json/new", endpoint.trim_end_matches('/'));
        let new_tab: Value = client_http
            .put(&new_url)
            .send()
            .await
            .map_err(|e| tool_fail(format!("Failed to open new tab: {e}")))?
            .json()
            .await
            .map_err(|e| tool_fail(format!("Invalid new tab response: {e}")))?;
        (
            new_tab["id"].as_str().unwrap_or("").to_string(),
            new_tab["webSocketDebuggerUrl"]
                .as_str()
                .ok_or_else(|| tool_fail("No webSocketDebuggerUrl in new tab".to_owned()))?
                .to_string(),
        )
    };

    if !tab_id.is_empty() {
        let act_url = format!(
            "{}/json/activate/{}",
            endpoint.trim_end_matches('/'),
            tab_id
        );
        let _ = client_http.put(&act_url).send().await;
    }

    let mut persistent = PersistentCdp::connect(endpoint, &tab_id, &ws_url)
        .await
        .map_err(|e| tool_fail(format!("CDP connection failed: {e}")))?;

    let _ = persistent.send("Page.enable", json!({}), cmd_timeout).await;
    let _ = persistent
        .send("Runtime.enable", json!({}), cmd_timeout)
        .await;
    let _ = persistent
        .send("Page.bringToFront", json!({}), cmd_timeout)
        .await;

    *guard = Some(persistent);
    Ok(guard)
}

// One arm per browser action, each with its own script and readback; the length
// is the action count rather than a tangled control flow.
#[allow(clippy::too_many_lines)]
async fn execute_browser_action(
    request: &BrowserRequest,
    limits: &BrowserLimits,
    cdp_slot: &Arc<Mutex<Option<PersistentCdp>>>,
) -> Result<BrowserOutput> {
    let req_bytes = serde_json::to_vec(request)?;
    let request_digest = Digest::of(&req_bytes);
    let cmd_timeout = Duration::from_secs(limits.cdp_timeout_seconds);

    let mut guard = get_or_create_cdp(&request.endpoint, cdp_slot, cmd_timeout).await?;
    let cdp = guard
        .as_mut()
        .ok_or_else(|| tool_fail("Failed to obtain persistent CDP client"))?;

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
        BrowserAction::ExtractDom => {
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
    };

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
        elements,
        screenshot_base64,
        request_digest,
    })
}
