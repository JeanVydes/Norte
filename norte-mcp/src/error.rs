//! Typed errors for the MCP client, server and transports.
//!
//! Every failure the crate can produce is one variant here. At the boundary where
//! a remote tool becomes a host [`AnyFunction`](norte_core::AnyFunction), the
//! error is lowered onto the core taxonomy so the engine sees one error type.

use serde_json::Value;

/// Convenience alias for results produced by this crate.
pub type McpResult<T> = std::result::Result<T, McpError>;

/// Anything that can go wrong while speaking MCP.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum McpError {
    /// The peer closed the transport, or a pending call was abandoned because the
    /// read loop ended.
    #[error("mcp transport closed")]
    Closed,

    /// The transport itself failed (spawn failure, broken pipe, framing error).
    #[error("mcp transport error: {0}")]
    Transport(String),

    /// An underlying `I/O` failure.
    #[error("mcp io error: {0}")]
    Io(#[from] std::io::Error),

    /// A message could not be encoded or decoded as `JSON`.
    #[error("mcp json error: {0}")]
    Json(#[from] serde_json::Error),

    /// The peer answered with a `JSON-RPC` error object.
    #[error("mcp rpc error {code}: {message}")]
    Rpc {
        /// The vendor or protocol error code.
        code: i64,
        /// The human-readable message.
        message: String,
        /// Any structured payload the peer attached.
        data: Option<Value>,
    },

    /// The peer sent a structurally valid message that violates the protocol.
    #[error("mcp protocol error: {0}")]
    Protocol(String),

    /// The peer does not implement the requested capability or method.
    #[error("mcp unsupported: {0}")]
    Unsupported(String),

    /// A base64 payload could not be decoded.
    #[error("mcp base64 error: {0}")]
    Base64(String),

    /// A core error surfaced while mapping MCP data onto the domain model.
    #[error(transparent)]
    Core(#[from] norte_core::Error),
}

impl McpError {
    /// Builds an RPC error from its parts.
    #[must_use]
    pub fn rpc(code: i64, message: impl Into<String>) -> Self {
        Self::Rpc {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// The RPC error code, when this failure came from a `JSON-RPC` error object.
    #[must_use]
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::Rpc { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Whether the peer reported that the method does not exist.
    #[must_use]
    pub fn is_method_not_found(&self) -> bool {
        self.code() == Some(crate::jsonrpc::codes::METHOD_NOT_FOUND)
    }

    /// Lowers this error onto the core taxonomy for a specific tool invocation.
    ///
    /// Errors that already carry core semantics (argument parsing, media
    /// decoding) pass through unchanged; everything else becomes a
    /// [`norte_core::Error::ToolFailed`] carrying the RPC code when present.
    #[must_use]
    pub fn into_core(self, tool: norte_core::ToolName) -> norte_core::Error {
        match self {
            Self::Core(error) => error,
            other => {
                let code = other.code().map(|code| code.to_string());
                norte_core::Error::ToolFailed {
                    tool,
                    reason: other.to_string(),
                    code,
                }
            }
        }
    }
}

impl From<base64::DecodeError> for McpError {
    fn from(error: base64::DecodeError) -> Self {
        Self::Base64(error.to_string())
    }
}
