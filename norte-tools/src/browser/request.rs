use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Actions an agent can ask the browser to execute.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BrowserAction {
    /// Navigate the active page to a URL allowed by the host's policy.
    Navigate {
        /// Target URL (e.g. `https://example.com`).
        #[schemars(example = &"https://example.com")]
        url: String,
    },
    /// Click an element by CSS selector or by an id from `extract_dom`.
    Click {
        /// CSS selector of the element to click.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Numeric agent id from a prior `extract_dom` call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<usize>,
    },
    /// Type text into an element, or into the focused element.
    Type {
        /// CSS selector for the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Numeric agent id from a prior `extract_dom` call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<usize>,
        /// Text to input.
        text: String,
        /// Whether to clear the field before typing.
        #[serde(default)]
        clear: bool,
    },
    /// Scroll the viewport.
    Scroll {
        /// Horizontal scroll delta in pixels.
        #[serde(default)]
        delta_x: f64,
        /// Vertical scroll delta in pixels.
        #[serde(default)]
        delta_y: f64,
    },
    /// Wait for a short, host-bounded duration.
    Wait {
        /// Seconds to sleep, capped by the host's limit.
        #[serde(default = "default_wait_seconds")]
        seconds: f64,
    },
    /// Map visible interactive elements with ids the next action can use.
    ExtractDom {
        /// The most elements to return; `0` returns none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            description = "The most elements to return. 0 returns none; omitting it uses the host's ceiling, and asking for more is clamped to it."
        )]
        max_elements: Option<usize>,
        /// Keeps elements whose label or destination contains this text.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            description = "Keep only elements whose label, name, placeholder or link destination contains this text, case-insensitively."
        )]
        filter: Option<String>,
        /// How many matching elements to skip before the first returned id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(
            description = "Skip this many matching elements before assigning ids. Use it with the total in the prior answer to page through a long list."
        )]
        offset: Option<usize>,
    },
    /// Read the page's text, or the text under a selector.
    ReadPage {
        /// CSS selector for the part of the page to read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Maximum characters to return, bounded by the host's limit.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_chars: Option<usize>,
    },
    /// Press one navigation or editing key.
    PressKey {
        /// The key to press.
        key: BrowserKey,
        /// CSS selector for the element to focus first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        /// Numeric agent id from a prior `extract_dom` call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<usize>,
    },
    /// Take a screenshot of the visible viewport as base64 PNG.
    Screenshot,
    /// List the tabs the browser already has open.
    ListTabs,
    /// Move the tool to a tab that is already open.
    SwitchTab {
        /// The target id, as `ListTabs` reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// A case-insensitive substring of the tab's URL or title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Open a new tab at a URL allowed by the host's policy.
    OpenTab {
        /// The URL to open, or a blank tab when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    /// Close a tab by id or by a URL/title substring.
    CloseTab {
        /// The target id, as `ListTabs` reported it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// A case-insensitive substring of the tab's URL or title.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
}

/// Keys a tool may press without handing raw key codes to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BrowserKey {
    /// Confirm or submit the focused control.
    Enter,
    /// Move focus to the next control.
    Tab,
    /// Dismiss a menu or dialog.
    Escape,
    /// Move the selection up.
    ArrowUp,
    /// Move the selection down.
    ArrowDown,
    /// Move the selection left.
    ArrowLeft,
    /// Move the selection right.
    ArrowRight,
    /// Delete the character before the caret.
    Backspace,
    /// Delete the character at the caret.
    Delete,
    /// Move to the start of the line.
    Home,
    /// Move to the end of the line.
    End,
    /// Move one page up.
    PageUp,
    /// Move one page down.
    PageDown,
    /// Insert a space.
    Space,
}

fn default_wait_seconds() -> f64 {
    1.0
}

/// Request sent to the `browser_use` tool.
///
/// The CDP endpoint and every limit live on the host's [`crate::BrowserLimits`],
/// not here. A model that could name the endpoint could drive a different
/// browser than the one the host chose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrowserRequest {
    /// Action to perform.
    pub action: BrowserAction,
    /// Highlight the target element before interacting. Off by default because
    /// it writes styles into the page the model is reading.
    #[serde(default)]
    pub highlight: bool,
    /// Snapshot token from the last `extract_dom`.
    ///
    /// Any action that uses `agent_id` must carry the token that assigned it,
    /// so a stale id cannot click a different control after the page changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Required with agent_id. Copy the snapshot from the last extract_dom so a stale id is rejected instead of clicking another control."
    )]
    pub snapshot: Option<String>,
}
