use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::id::{ApprovalId, BlobId, GrantId, ModelRef, NodeId, ProviderId, SessionId, ToolName};

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Common provider taxonomy. Every vendor wire error is mapped onto exactly one
/// of these kinds; the vendor code, message and any extra payload survive in
/// [`ProviderError`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderErrorKind {
    /// The request was throttled.
    RateLimited,
    /// The account is out of quota or a hard billing limit was reached. Unlike a
    /// rate limit this never clears on its own, so it must not be retried.
    QuotaExceeded,
    /// The request was rejected as malformed or unsupported.
    InvalidRequest,
    /// The credential is missing or invalid.
    Authentication,
    /// The credential is valid but is not allowed to do this.
    Permission,
    /// The addressed resource does not exist.
    NotFound,
    /// The provider refused the content.
    ContentFilter,
    /// The model is temporarily overloaded.
    ModelOverloaded,
    /// The provider failed on its side.
    ServerError,
    /// The connection failed before or during the exchange.
    Transport,
    /// The response could not be decoded.
    Decode,
    /// The caller cancelled.
    Cancelled,
    /// A failure that fits no other kind.
    Other,
}

/// A provider failure, normalized but lossless.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProviderError {
    /// The normalized kind.
    pub kind: ProviderErrorKind,
    /// The provider's message, as it sent it.
    pub message: String,
    /// The provider's own error code, where it has one.
    pub code: Option<String>,
    /// The HTTP status, where there was one.
    pub status: Option<u16>,
    /// How long the provider asked the caller to wait.
    pub retry_after: Option<Duration>,
    /// Anything else the payload carried, kept verbatim.
    pub extensions: BTreeMap<String, Value>,
}

impl ProviderError {
    /// A failure of one kind, with its message and nothing else.
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

    /// Sets the provider's own error code.
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    /// Sets the HTTP status the provider answered with.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets how long the provider asked the caller to wait.
    #[must_use]
    pub fn with_retry_after(mut self, retry_after: Duration) -> Self {
        self.retry_after = Some(retry_after);
        self
    }

    /// Keeps one more field of the provider's payload under its own key.
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

/// Everything the crate can fail with.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum Error {
    /// An argument the caller supplied is not valid.
    #[error("invalid input: {0}")]
    InvalidInput(String),

    /// A layer refused the request before it reached the model.
    ///
    /// The reason is typed so a runtime can put it in the audit trail: a layer
    /// wraps one model call and knows nothing about events or sessions, so it
    /// reports *why* it stopped here and lets the caller decide what to record.
    #[error("the {} layer short-circuited the request: {reason:?}", layer)]
    ShortCircuited {
        /// The layer's name, as it appears in the audit trail.
        layer: String,
        /// Why it stopped the request.
        reason: crate::layer::ShortCircuitReason,
    },

    /// An identifier could not be parsed.
    #[error("invalid identifier: {0}")]
    InvalidId(String),

    /// A document carries a schema version this build does not know.
    #[error("invalid schema version {found} for {kind}")]
    SchemaVersion {
        /// The document kind.
        kind: String,
        /// The version the document carries.
        found: u16,
    },

    /// A stored phase name is not one this build knows.
    #[error("unknown phase: {0}")]
    UnknownPhase(String),

    /// A stored node kind is not one this build knows.
    #[error("unknown node kind: {0}")]
    UnknownNodeKind(String),

    /// A commit lost a race: the stored session moved.
    #[error("store conflict on session {session}: expected {expected:?}, found {actual}")]
    StoreConflict {
        /// The session being committed.
        session: SessionId,
        /// The version the commit was built from.
        expected: Option<u64>,
        /// The version the store holds.
        actual: u64,
    },

    /// No session with that id.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),

    /// No node with that id.
    #[error("node not found: {0}")]
    NodeNotFound(NodeId),

    /// No blob with that id.
    #[error("blob not found: {0}")]
    BlobNotFound(BlobId),

    /// A file the provider holds cannot be fetched.
    #[error("provider file {provider}/{file_id} is unavailable: {reason}")]
    ProviderFileUnavailable {
        /// The provider that holds the file.
        provider: ProviderId,
        /// The provider's id for the file.
        file_id: String,
        /// Why it is unavailable.
        reason: String,
    },

    /// No approval record with that id.
    #[error("approval not found: {0}")]
    ApprovalNotFound(ApprovalId),

    /// The approval's time to live elapsed before a decision was made.
    #[error("approval expired: {0}")]
    ApprovalExpired(ApprovalId),

    /// A human denied the call.
    #[error("approval denied: {0}")]
    ApprovalDenied(String),

    /// No grant with that id.
    #[error("grant not found: {0}")]
    GrantNotFound(GrantId),

    /// The model cannot serve something the request needs.
    #[error("capability mismatch: model {model} does not support {feature}")]
    Capability {
        /// The model the request was built for.
        model: ModelRef,
        /// The feature it needs.
        feature: String,
    },

    /// The provider's continuity material cannot be replayed safely.
    #[error("continuity blocked: {0}")]
    ContinuityBlocked(String),

    /// The model called a tool the registry does not have.
    #[error("tool not found: {0}")]
    ToolNotFound(ToolName),

    /// The arguments the model sent do not parse into the tool's type.
    #[error("invalid arguments for tool {tool}: {reason}")]
    InvalidToolArguments {
        /// The tool.
        tool: ToolName,
        /// What did not fit.
        reason: String,
    },

    /// The tool ran and failed.
    #[error("tool {tool} failed: {reason}")]
    ToolFailed {
        /// The tool.
        tool: ToolName,
        /// Why it failed.
        reason: String,
        /// The tool's own error code, where it has one.
        code: Option<String>,
    },

    /// The tool did not finish within its timeout.
    #[error("tool {tool} timed out after {timeout:?}")]
    ToolTimeout {
        /// The tool.
        tool: ToolName,
        /// The timeout it exceeded.
        timeout: Duration,
    },

    /// Something exceeded a configured bound.
    #[error("{what} exceeds the configured limit of {limit}")]
    LimitExceeded {
        /// What exceeded it.
        what: String,
        /// The bound.
        limit: usize,
    },

    /// The caller cancelled.
    #[error("canceled")]
    Cancelled,

    /// The provider failed.
    #[error("provider error: {0}")]
    Provider(Box<ProviderError>),

    /// A document could not be encoded or decoded.
    #[error("codec error: {0}")]
    Codec(String),

    /// The filesystem or another IO resource failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A JSON document could not be encoded or decoded.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A failure that fits no other variant.
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Wraps a provider failure.
    #[must_use]
    pub fn provider(error: ProviderError) -> Self {
        Self::Provider(Box::new(error))
    }

    /// The normalized provider kind, when this is a provider failure.
    #[must_use]
    pub fn provider_kind(&self) -> Option<ProviderErrorKind> {
        match self {
            Error::Provider(error) => Some(error.kind),
            _ => None,
        }
    }

    /// How long the provider asked the caller to wait, when it asked.
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
