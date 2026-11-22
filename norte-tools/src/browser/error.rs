use std::fmt;

use norte_core::Error;
use tokio_tungstenite::tungstenite::Error as WebSocketError;

pub(super) const TOOL_NAME: &str = "browser_use";

/// The tool's own failure categories, carried on `Error::ToolFailed.code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BrowserErrorCode {
    /// A URL fell outside the host's navigation policy.
    UrlPolicy,
    /// The WebSocket could not carry a command.
    CdpTransport,
    /// A command or event did not arrive in time.
    CdpTimeout,
    /// The browser rejected a CDP command.
    CdpProtocol,
    /// The CDP connection is no longer usable.
    CdpClosed,
    /// JavaScript in the inspected page threw.
    PageScript,
    /// The browser refused a navigation.
    Navigation,
    /// The page changed while an element id was being resolved.
    ElementSnapshot,
}

impl BrowserErrorCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::UrlPolicy => "url_policy",
            Self::CdpTransport => "cdp_transport",
            Self::CdpTimeout => "cdp_timeout",
            Self::CdpProtocol => "cdp_protocol",
            Self::CdpClosed => "cdp_closed",
            Self::PageScript => "page_script",
            Self::Navigation => "navigation",
            Self::ElementSnapshot => "element_snapshot",
        }
    }
}

fn tool_failure(reason: impl Into<String>, code: Option<BrowserErrorCode>) -> Error {
    // The name is a compile-time constant of this crate and passes the
    // identifier rules the constructor checks.
    let tool = norte_core::ToolName::new(TOOL_NAME).expect("browser_use is a valid tool name");
    Error::ToolFailed {
        tool,
        reason: reason.into(),
        code: code.map(BrowserErrorCode::as_str).map(str::to_owned),
    }
}

pub(super) fn tool_fail(reason: impl Into<String>) -> Error {
    tool_failure(reason, None)
}

pub(super) fn tool_fail_code(reason: impl Into<String>, code: BrowserErrorCode) -> Error {
    tool_failure(reason, Some(code))
}

/// Reports whether a failure requires a new CDP connection before a retry.
pub(super) fn is_connection_error(error: &Error) -> bool {
    matches!(
        error,
        Error::ToolFailed {
            code: Some(code),
            ..
        } if matches!(
            code.as_str(),
            "cdp_transport" | "cdp_closed" | "cdp_timeout"
        )
    )
}

#[derive(Debug)]
pub(super) enum CdpError {
    Transport(String),
    Timeout(String),
    Protocol {
        method: &'static str,
        message: String,
    },
    Closed,
}

impl CdpError {
    pub(super) fn into_tool_error(self) -> Error {
        let code = match &self {
            Self::Transport(_) => BrowserErrorCode::CdpTransport,
            Self::Timeout(_) => BrowserErrorCode::CdpTimeout,
            Self::Protocol { .. } => BrowserErrorCode::CdpProtocol,
            Self::Closed => BrowserErrorCode::CdpClosed,
        };
        tool_fail_code(self.to_string(), code)
    }
}

impl fmt::Display for CdpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(reason) => write!(formatter, "CDP transport failed: {reason}"),
            Self::Timeout(reason) => formatter.write_str(reason),
            Self::Protocol { method, message } => {
                write!(formatter, "CDP command {method} failed: {message}")
            }
            Self::Closed => formatter.write_str("the CDP connection is closed"),
        }
    }
}

impl std::error::Error for CdpError {}

impl From<WebSocketError> for CdpError {
    fn from(error: WebSocketError) -> Self {
        Self::Transport(error.to_string())
    }
}
