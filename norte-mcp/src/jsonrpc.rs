//! The `JSON-RPC` 2.0 envelope MCP is built on.
//!
//! Only the three shapes MCP uses are modelled: a request (id + method), a
//! notification (method, no id) and a response (id + result or error). The
//! [`Message`] union classifies any decoded value by inspecting those fields, so
//! a malformed or unknown envelope becomes a typed error rather than a silent
//! mis-decode.

use std::fmt::{Display, Formatter};

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

use crate::error::{McpError, McpResult};

/// The only protocol revision this crate emits.
pub const JSONRPC_VERSION: &str = "2.0";

/// Standard `JSON-RPC` error codes, plus the reserved range rule.
pub mod codes {
    /// Invalid `JSON` was received.
    pub const PARSE_ERROR: i64 = -32700;
    /// The request was not a valid request object.
    pub const INVALID_REQUEST: i64 = -32600;
    /// The method does not exist or is not available.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// The method parameters are invalid.
    pub const INVALID_PARAMS: i64 = -32602;
    /// An internal error occurred.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// A request or response identifier: a number or a string, never null.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    /// An integer identifier, used by this crate.
    Number(i64),
    /// A string identifier, accepted from peers.
    String(String),
}

impl Display for RequestId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(value) => write!(f, "{value}"),
            Self::String(value) => f.write_str(value),
        }
    }
}

/// A request: it carries an id and expects exactly one response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// Builds a protocol-version request.
    #[must_use]
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A notification: a method invocation with no id and no response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Builds a protocol-version notification.
    #[must_use]
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            method: method.into(),
            params,
        }
    }
}

/// A `JSON-RPC` error object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RpcError {
    /// The protocol or application error code.
    pub code: i64,
    /// The human-readable message.
    pub message: String,
    /// Optional structured payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// Builds an error with an optional data payload.
    #[must_use]
    pub fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            code,
            message: message.into(),
            data,
        }
    }
}

impl From<McpError> for RpcError {
    fn from(error: McpError) -> Self {
        match error {
            McpError::Rpc {
                code,
                message,
                data,
            } => Self {
                code,
                message,
                data,
            },
            McpError::Unsupported(message) | McpError::Protocol(message) => {
                Self::new(codes::METHOD_NOT_FOUND, message, None)
            }
            other => Self::new(codes::INTERNAL_ERROR, other.to_string(), None),
        }
    }
}

/// A response to a [`Request`]: exactly one of the two payloads is present.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    /// A successful response carrying a result.
    #[must_use]
    pub fn success(id: RequestId, result: Value) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// An error response.
    #[must_use]
    pub fn error(id: RequestId, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            result: None,
            error: Some(error),
        }
    }

    /// Unwraps the result, converting an error object into [`McpError::Rpc`].
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Rpc`] when the response carries an error object.
    pub fn into_result(self) -> McpResult<Value> {
        if let Some(error) = self.error {
            return Err(McpError::Rpc {
                code: error.code,
                message: error.message,
                data: error.data,
            });
        }
        Ok(self.result.unwrap_or(Value::Null))
    }
}

/// Any message that can travel over a transport.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A request awaiting a response.
    Request(Request),
    /// A response to an earlier request.
    Response(Response),
    /// A one-way notification.
    Notification(Notification),
}

impl Message {
    /// Classifies a decoded `JSON` value, distinguishing the three shapes.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Protocol`] when the value has neither a method nor an
    /// id, or when its required fields are missing or malformed.
    pub fn from_value(value: Value) -> McpResult<Self> {
        let method = value.get("method").and_then(Value::as_str);
        let has_id = value.get("id").is_some_and(|id| !id.is_null());
        match (method, has_id) {
            (Some(_), true) => serde_json::from_value(value)
                .map(Self::Request)
                .map_err(McpError::from),
            (Some(_), false) => serde_json::from_value(value)
                .map(Self::Notification)
                .map_err(McpError::from),
            (None, true) => serde_json::from_value(value)
                .map(Self::Response)
                .map_err(McpError::from),
            (None, false) => Err(McpError::Protocol(
                "message has neither a method nor an id".to_owned(),
            )),
        }
    }

    /// The request id, when the message is a request or a response.
    #[must_use]
    pub fn id(&self) -> Option<&RequestId> {
        match self {
            Self::Request(request) => Some(&request.id),
            Self::Response(response) => Some(&response.id),
            Self::Notification(_) => None,
        }
    }
}

impl From<Request> for Message {
    fn from(value: Request) -> Self {
        Self::Request(value)
    }
}

impl From<Response> for Message {
    fn from(value: Response) -> Self {
        Self::Response(value)
    }
}

impl From<Notification> for Message {
    fn from(value: Notification) -> Self {
        Self::Notification(value)
    }
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Request(value) => value.serialize(serializer),
            Self::Response(value) => value.serialize(serializer),
            Self::Notification(value) => value.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for Message {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(value).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn requests_round_trip() {
        let request = Request::new(RequestId::Number(1), "tools/list", None);
        let encoded = serde_json::to_value(&request).expect("encode");
        assert_eq!(encoded["jsonrpc"], "2.0");
        assert_eq!(encoded["method"], "tools/list");
        assert!(encoded.get("params").is_none());

        let decoded: Message = serde_json::from_value(encoded).expect("decode");
        assert_eq!(decoded, Message::Request(request));
    }

    #[test]
    fn notifications_have_no_id() {
        let decoded = Message::from_value(json!({ "jsonrpc": "2.0", "method": "ping" }))
            .expect("notification");
        assert!(matches!(decoded, Message::Notification(_)));
        assert!(decoded.id().is_none());
    }

    #[test]
    fn responses_carry_results_or_errors() {
        let ok = Message::from_value(json!({ "jsonrpc": "2.0", "id": 7, "result": { "x": 1 } }))
            .expect("response");
        match ok {
            Message::Response(response) => {
                assert_eq!(response.into_result().expect("result"), json!({ "x": 1 }));
            }
            other => panic!("unexpected message: {other:?}"),
        }

        let failure = Response::error(RequestId::Number(7), RpcError::new(-32601, "nope", None));
        let error = failure.into_result().expect_err("error");
        assert_eq!(error.code(), Some(-32601));
    }

    #[test]
    fn an_idless_error_answer_is_rejected() {
        let error = Message::from_value(
            json!({ "jsonrpc": "2.0", "error": { "code": 1, "message": "x" } }),
        )
        .expect_err("rejected");
        assert!(matches!(error, McpError::Protocol(_)));
    }

    #[test]
    fn null_id_is_a_notification() {
        let decoded = Message::from_value(json!({ "jsonrpc": "2.0", "id": null, "method": "x" }))
            .expect("notification");
        assert!(matches!(decoded, Message::Notification(_)));
    }

    #[test]
    fn string_ids_are_supported() {
        let decoded = Message::from_value(json!({ "jsonrpc": "2.0", "id": "abc", "method": "x" }))
            .expect("request");
        assert_eq!(decoded.id(), Some(&RequestId::String("abc".to_owned())));
    }
}
