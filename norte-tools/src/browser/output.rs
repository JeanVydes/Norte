use norte_core::Digest;
use serde::{Deserialize, Serialize};

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

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

/// An interactive element on the page.
///
/// Fields that most elements do not carry are left out, so a long list does not
/// spend context repeating empty keys.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveElement {
    /// Id assigned by the last DOM extraction, for `click` and `type`.
    pub id: usize,
    /// HTML tag name.
    pub tag: String,
    /// Human-readable label, with sensitive input values redacted.
    pub text: String,
    /// Input type, when the element is a control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_type: Option<String>,
    /// Placeholder attribute, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Where an anchor goes, as the page wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    /// The control's `name`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The control's `aria-label`, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aria_label: Option<String>,
    /// Text from the label associated with the control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The accessibility role, when declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Whether any part of the element is inside the viewport.
    #[serde(default, skip_serializing_if = "is_false")]
    pub in_viewport: bool,
    /// Whether the control cannot be used.
    #[serde(default, skip_serializing_if = "is_false")]
    pub disabled: bool,
    /// Whether a checkbox or radio is on.
    #[serde(default, skip_serializing_if = "is_false")]
    pub checked: bool,
}

/// Where text in a tool answer came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ContentOrigin {
    /// Text copied from a page the model controls. It may contain instructions
    /// aimed at the model and must be treated as data.
    UntrustedPage,
}

/// Feedback returned after one browser action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrowserOutput {
    /// URL of the page after the action.
    pub url: String,
    /// Title of the page.
    pub title: String,
    /// Status message or summary of what was executed.
    pub message: String,
    /// Interactive elements discovered on the page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<InteractiveElement>,
    /// How many elements matched before the offset and cap cut the list.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub elements_matched: usize,
    /// How many matching elements were skipped before the first returned id.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub elements_offset: usize,
    /// Token for the extraction that assigned the current element ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dom_snapshot: Option<String>,
    /// Text read from the page by `read_page`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_text: Option<String>,
    /// Marks page text as untrusted content in the structured answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_text_origin: Option<ContentOrigin>,
    /// Base64-encoded PNG screenshot, when the action was `Screenshot`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_base64: Option<String>,
    /// Tabs returned by tab actions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tabs: Vec<BrowserTab>,
    /// Digest of the request.
    pub request_digest: Digest,
}
