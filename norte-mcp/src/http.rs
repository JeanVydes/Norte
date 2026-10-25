//! The Streamable HTTP transport.
//!
//! MCP over HTTP is not a duplex pipe, so this does not pretend to be one. Every
//! message the client sends is one POST, and the server answers that POST either
//! with a single JSON object or with an SSE stream that carries the answer and
//! whatever the server wants to say in the meantime. A GET opens a second stream
//! for the messages the server starts by itself.
//!
//! The pair of queues in the middle is what bridges the two shapes: `send`
//! performs the round trip and pushes what comes back, `recv` hands it out in
//! order. That keeps the [`Transport`] contract — one reader, one writer, both
//! concurrent — without inventing a pipe that HTTP does not have.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};

use super::Transport;
use crate::error::{McpError, McpResult};
use crate::jsonrpc::Message;

/// The header a server uses to name the session it opened.
const SESSION_HEADER: &str = "mcp-session-id";
/// The header a client uses to resume a stream it lost.
const LAST_EVENT_HEADER: &str = "last-event-id";
/// What a POST accepts: one JSON object, or a stream of events.
const ACCEPT_BOTH: &str = "application/json, text/event-stream";
/// What a GET accepts: only a stream of events.
const ACCEPT_STREAM: &str = "text/event-stream";

/// A client speaking MCP to one endpoint over Streamable HTTP.
pub struct HttpTransport {
    client: Client,
    endpoint: String,
    /// The headers every request carries, so a token is set once.
    headers: HeaderMap,
    /// The session the server handed out, once it has.
    session: Mutex<Option<String>>,
    /// Messages waiting to be handed to `recv`, in the order they arrived.
    incoming: Mutex<mpsc::Receiver<Message>>,
    /// The writing end of [`HttpTransport::incoming`], cloned into the tasks.
    sender: mpsc::Sender<Message>,
    /// The last event id seen on each stream, for a resume.
    last_event: Mutex<Option<String>>,
    /// Whether the background GET stream is running.
    streaming: AtomicBool,
    /// How long one request may take.
    request_timeout: Duration,
}

impl HttpTransport {
    /// A transport that will speak to `endpoint`.
    ///
    /// Nothing is sent until the client does its handshake.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the HTTP client cannot be built, which is
    /// a failure of the TLS backend rather than of the endpoint.
    pub fn new(endpoint: impl Into<String>) -> McpResult<Self> {
        let client = Client::builder()
            .build()
            .map_err(|error| McpError::Transport(format!("could not build a client: {error}")))?;
        let (sender, receiver) = mpsc::channel(64);
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            headers: HeaderMap::new(),
            session: Mutex::new(None),
            incoming: Mutex::new(receiver),
            sender,
            last_event: Mutex::new(None),
            streaming: AtomicBool::new(false),
            request_timeout: Duration::from_secs(60),
        })
    }

    /// Adds a header every request carries, such as an authorization token.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the value is not a legal header value,
    /// which is what a token with a stray newline would be.
    pub fn with_header(mut self, name: &str, value: &str) -> McpResult<Self> {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| McpError::Transport(format!("bad header name `{name}`: {error}")))?;
        let value = HeaderValue::from_str(value)
            .map_err(|error| McpError::Transport(format!("bad header value: {error}")))?;
        self.headers.insert(name, value);
        Ok(self)
    }

    /// Starts the stream the server uses for messages it begins itself.
    ///
    /// Call it once the handshake has named the session. A server that offers no
    /// stream answers 405 and nothing is kept open.
    pub fn start_stream(self: &Arc<Self>) {
        if self.request_timeout.is_zero() {
            return;
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.open_stream().await;
        });
    }

    /// Sets the deadline for one request.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    /// Sends one message and pushes whatever the server answers.
    async fn post(&self, message: Message) -> McpResult<()> {
        let mut request = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
            .header(ACCEPT, ACCEPT_BOTH)
            .header(CONTENT_TYPE, "application/json")
            .timeout(self.request_timeout)
            .body(serde_json::to_string(&message)?);
        if let Some(session) = self.session.lock().await.clone() {
            request = request.header(SESSION_HEADER, session);
        }

        let response = request
            .send()
            .await
            .map_err(|error| McpError::Transport(format!("request failed: {error}")))?;
        let status = response.status();

        // The server names the session once, on the answer to `initialize`.
        if let Some(session) = response.headers().get(SESSION_HEADER) {
            if let Ok(session) = session.to_str() {
                *self.session.lock().await = Some(session.to_owned());
            }
        }

        // A notification or a response is accepted with no body at all.
        if status == StatusCode::ACCEPTED {
            return Ok(());
        }
        if status == StatusCode::NOT_FOUND {
            // The session expired; forgetting it lets the next handshake open a
            // new one, which is what the specification asks a client to do.
            *self.session.lock().await = None;
            return Err(McpError::Transport(
                "the server ended the session; a new handshake is needed".to_owned(),
            ));
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(McpError::Transport(format!(
                "the server answered {status}: {}",
                body.trim()
            )));
        }

        let stream = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        if stream {
            self.read_sse(response).await
        } else {
            // One JSON object, which may be a response or a server request.
            let value: Value = response
                .json()
                .await
                .map_err(|error| McpError::Transport(format!("bad answer body: {error}")))?;
            self.deliver(Message::from_value(value)?).await;
            Ok(())
        }
    }

    /// Reads an SSE stream to its end, pushing every message it carries.
    async fn read_sse(&self, response: reqwest::Response) -> McpResult<()> {
        let mut buffer = String::new();
        let mut stream = response;
        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|error| McpError::Transport(format!("stream failed: {error}")))?
        {
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            // Events are separated by a blank line; a partial one stays in the
            // buffer for the next chunk.
            while let Some(index) = buffer.find("\n\n") {
                let block = buffer[..index].to_owned();
                buffer.drain(..index + 2);
                let mut event = SseEvent::default();
                if let Some(id) = event.parse(&block) {
                    *self.last_event.lock().await = Some(id);
                }
                if let Some(message) = event.into_message()? {
                    self.deliver(message).await;
                }
            }
        }
        Ok(())
    }

    /// Pushes one message to the reader, or reports the channel closed.
    async fn deliver(&self, message: Message) {
        let _ = self.sender.send(message).await;
    }

    /// Opens the stream the server uses for messages it starts itself.
    ///
    /// A server that offers none answers 405, which is not an error: it simply
    /// has nothing to say on its own.
    async fn open_stream(self: &Arc<Self>) {
        if self.streaming.swap(true, Ordering::SeqCst) {
            return;
        }
        while self.streaming.load(Ordering::SeqCst) {
            if let Err(error) = self.get_stream().await {
                tracing::debug!(%error, "the MCP server stream ended");
            }
            if !self.streaming.load(Ordering::SeqCst) {
                break;
            }
            // A dropped stream is resumed after a pause, with the last event id
            // so the server can replay what was missed.
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// One GET of the server's stream, until it ends or fails.
    async fn get_stream(&self) -> McpResult<()> {
        let mut request = self
            .client
            .get(&self.endpoint)
            .headers(self.headers.clone())
            .header(ACCEPT, ACCEPT_STREAM);
        if let Some(session) = self.session.lock().await.clone() {
            request = request.header(SESSION_HEADER, session);
        }
        if let Some(last) = self.last_event.lock().await.clone() {
            request = request.header(LAST_EVENT_HEADER, last);
        }
        let response = request
            .send()
            .await
            .map_err(|error| McpError::Transport(format!("stream request failed: {error}")))?;
        if response.status() == StatusCode::METHOD_NOT_ALLOWED {
            // The server does not offer a stream: nothing to keep open.
            self.streaming.store(false, Ordering::SeqCst);
            return Ok(());
        }
        if !response.status().is_success() {
            return Err(McpError::Transport(format!(
                "the stream answered {}",
                response.status()
            )));
        }
        self.read_sse(response).await
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(&self, message: Message) -> McpResult<()> {
        self.post(message).await
    }

    async fn recv(&self) -> McpResult<Option<Message>> {
        Ok(self.incoming.lock().await.recv().await)
    }

    async fn close(&self) -> McpResult<()> {
        self.streaming.store(false, Ordering::SeqCst);
        // A session the client no longer needs is ended explicitly, which the
        // specification prefers over leaving it to time out.
        if let Some(session) = self.session.lock().await.clone() {
            let _ = self
                .client
                .delete(&self.endpoint)
                .headers(self.headers.clone())
                .header(SESSION_HEADER, session)
                .timeout(self.request_timeout)
                .send()
                .await;
        }
        Ok(())
    }
}

/// One parsed SSE event: its id, its type and its data lines.
#[derive(Default)]
struct SseEvent {
    id: Option<String>,
    data: Vec<String>,
}

impl SseEvent {
    /// Fills the event from one block, returning the id it carried.
    fn parse(&mut self, block: &str) -> Option<String> {
        for line in block.lines() {
            let line = line.trim_end();
            if let Some(value) = line.strip_prefix("id:") {
                self.id = Some(value.trim().to_owned());
            } else if let Some(value) = line.strip_prefix("data:") {
                self.data.push(value.trim_start().to_owned());
            }
            // `event:` names the type; MCP carries its own `method`, so it is
            // not needed here, and a comment line starts with a colon.
        }
        self.id.clone()
    }

    /// The message the event carries, when it carries one.
    fn into_message(self) -> McpResult<Option<Message>> {
        if self.data.is_empty() {
            return Ok(None);
        }
        let joined = self.data.join("\n");
        if joined.trim().is_empty() {
            return Ok(None);
        }
        let value: Value = serde_json::from_str(&joined)?;
        Message::from_value(value).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_carries_its_id_and_its_data() {
        let mut event = SseEvent::default();
        let id = event.parse("id: 42\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}");
        assert_eq!(id.as_deref(), Some("42"));
        let message = event.into_message().expect("decode").expect("a message");
        assert!(matches!(message, Message::Response(_)), "{message:?}");
    }

    #[test]
    fn data_lines_are_joined_with_newlines() {
        // The standard says a multi-line data field is the lines joined by
        // newline, which matters because a JSON body may be pretty-printed.
        let mut event = SseEvent::default();
        event.parse("data: {\"jsonrpc\":\"2.0\",\ndata: \"id\":1,\"result\":null}");
        let message = event.into_message().expect("decode").expect("a message");
        assert!(matches!(message, Message::Response(_)), "{message:?}");
    }

    #[test]
    fn an_event_without_data_is_not_a_message() {
        // A keep-alive or a comment is an event with nothing in it, and it must
        // not be reported as a failure.
        let mut event = SseEvent::default();
        event.parse(": keep-alive");
        assert!(event.into_message().expect("decode").is_none());
    }

    #[test]
    fn an_event_without_an_id_resumes_from_whatever_came_before() {
        let mut event = SseEvent::default();
        assert_eq!(event.parse("data: {}"), None);
    }
}
