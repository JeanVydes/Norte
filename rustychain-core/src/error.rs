use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::id::{ApprovalId, BlobId, ModelRef, NodeId, ProviderId, SessionId, ToolName};

pub type Result<T> = std::result::Result<T, Error>;

/// Common provider taxonomy. Every vendor wire error is mapped onto exactly one
/// of these kinds; the vendor code, message and any extra payload survive in
/// [`ProviderError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderErrorKind {
    RateLimited,
    /// The account is out of quota or a hard billing limit was reached. Unlike a
    /// rate limit this never clears on its own, so it must not be retried.
    QuotaExceeded,
    InvalidRequest,
    Authentication,
    Permission,
    NotFound,
    ContentFilter,
    ModelOverloaded,
    ServerError,
    Transport,
    Decode,
    Cancelled,
    Other,
}

/// A provider failure, normalized but lossless.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub message: String,
    pub code: Option<String>,
    pub status: Option<u16>,
    pub retry_after: Option<Duration>,
    pub extensions: BTreeMap<String, Value>,
}

impl ProviderError {
    pub fn new(kind: ProviderErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            code: None,
            status: None,
            retry_after: None,
            extensions: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    #[must_use]
    pub fn with_extension(mut self, key: impl Into<String>, value: Value) -> Self {
        self.extensions.insert(key.into(), value);
        self
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)?;
        if let Some(code) = &self.code {
            write!(f, " (code {code})")?;
        }
        if let Some(status) = self.status {
            write!(f, " (status {status})")?;
        }
        Ok(())
    }
}

impl std::error::Error for ProviderError {}

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    #[error("invalid input: {0}")]
    InvalidInput(String),

    #[error("invalid identifier: {0}")]
    InvalidId(String),

    #[error("invalid schema version {found} for {kind}")]
    SchemaVersion { kind: String, found: u16 },

    #[error("unknown phase: {0}")]
    UnknownPhase(String),

    #[error("unknown node kind: {0}")]
    UnknownNodeKind(String),

    #[error("store conflict on session {session}: expected {expected:?}, found {actual}")]
    StoreConflict {
        session: SessionId,
        expected: Option<u64>,
        actual: u64,
    },

    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    #[error("node not found: {0}")]
    NodeNotFound(NodeId),

    #[error("blob not found: {0}")]
    BlobNotFound(BlobId),

    #[error("provider file {provider}/{file_id} is unavailable: {reason}")]
    ProviderFileUnavailable {
        provider: ProviderId,
        file_id: String,
        reason: String,
    },

    #[error("approval not found: {0}")]
    ApprovalNotFound(ApprovalId),

    #[error("approval expired: {0}")]
    ApprovalExpired(ApprovalId),

    #[error("approval denied: {0}")]
    ApprovalDenied(String),

    #[error("capability mismatch: model {model} does not support {feature}")]
    Capability { model: ModelRef, feature: String },

    #[error("continuity blocked: {0}")]
    ContinuityBlocked(String),

    #[error("tool not found: {0}")]
    ToolNotFound(ToolName),

    #[error("invalid arguments for tool {tool}: {reason}")]
    InvalidToolArguments { tool: ToolName, reason: String },

    #[error("tool {tool} failed: {reason}")]
    ToolFailed {
        tool: ToolName,
        reason: String,
        code: Option<String>,
    },

    #[error("tool {tool} timed out after {timeout:?}")]
    ToolTimeout { tool: ToolName, timeout: Duration },

    #[error("{what} exceeds the configured limit of {limit}")]
    LimitExceeded { what: String, limit: usize },

    #[error("canceled")]
    Cancelled,

    #[error("provider error: {0}")]
    Provider(Box<ProviderError>),

    #[error("codec error: {0}")]
    Codec(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("{0}")]
    Other(String),
}

impl Error {
    #[must_use]
    pub fn provider(error: ProviderError) -> Self {
        Self::Provider(Box::new(error))
    }

    #[must_use]
    pub fn provider_kind(&self) -> Option<ProviderErrorKind> {
        match self {
            Error::Provider(error) => Some(error.kind),
            _ => None,
        }
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Provider(error) => error.retry_after,
            _ => None,
        }
    }
}

impl From<ProviderError> for Error {
    fn from(error: ProviderError) -> Self {
        Error::Provider(Box::new(error))
    }
}
