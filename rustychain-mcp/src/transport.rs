//! Message transports.
//!
//! MCP is transport-agnostic; this crate ships the two that need no extra
//! dependency. [`StdioTransport`] speaks the newline-delimited `JSON` framing of
//! a child process, and [`ChannelTransport`] is a fully in-process pair used for
//! embedding and for tests. Both share one [`Transport`] trait, so a host that
//! needs a different channel (a socket, a browser bridge) can implement it
//! without touching the client or the server.

use std::ffi::OsStr;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::error::{McpError, McpResult};
use crate::jsonrpc::Message;

/// A bidirectional stream of MCP messages.
///
/// Implementations must allow one concurrent reader and one concurrent writer:
/// the client drives a read loop while calls send, so `send` and `recv` may run
/// at the same time.
#[async_trait]
pub trait Transport: Send + Sync + 'static {
    /// Encodes and writes one message.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the message cannot be encoded or the
    /// underlying channel is closed.
    async fn send(&self, message: Message) -> McpResult<()>;

    /// Reads the next message, or `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns a transport or protocol error when a frame cannot be read or
    /// decoded.
    async fn recv(&self) -> McpResult<Option<Message>>;

    /// Releases any resources held by the transport.
    ///
    /// # Errors
    ///
    /// Returns a transport error when the channel cannot be closed cleanly.
    async fn close(&self) -> McpResult<()> {
        Ok(())
    }
}

/// Reads one newline-delimited `JSON-RPC` frame, skipping blank keep-alive lines.
///
/// # Errors
///
/// Returns [`McpError::Io`] on a read failure and a `JSON` or protocol error when
/// the frame is not a valid `JSON-RPC` message.
pub(crate) async fn read_framed<R>(reader: &mut R) -> McpResult<Option<Message>>
where
    R: AsyncBufRead + Unpin,
{
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return Ok(None);
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(trimmed)?;
        return Message::from_value(value).map(Some);
    }
}

/// Writes one message followed by a newline.
///
/// # Errors
///
/// Returns an error when the message cannot be encoded or the write fails.
pub(crate) async fn write_framed<W>(writer: &mut W, message: &Message) -> McpResult<()>
where
    W: AsyncWrite + Unpin,
{
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    writer.write_all(line.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}

/// An in-process transport pair.
///
/// [`ChannelTransport::pair`] returns two ends; each sends to the other. It is the
/// transport used by the crate's tests and by hosts that embed an MCP server
/// directly in the same process.
pub struct ChannelTransport {
    outgoing: tokio::sync::mpsc::UnboundedSender<Message>,
    incoming: Mutex<tokio::sync::mpsc::UnboundedReceiver<Message>>,
}

impl ChannelTransport {
    /// Creates a connected pair of transports.
    #[must_use]
    pub fn pair() -> (Self, Self) {
        let (left_tx, left_rx) = tokio::sync::mpsc::unbounded_channel();
        let (right_tx, right_rx) = tokio::sync::mpsc::unbounded_channel();
        (
            Self {
                outgoing: left_tx,
                incoming: Mutex::new(right_rx),
            },
            Self {
                outgoing: right_tx,
                incoming: Mutex::new(left_rx),
            },
        )
    }
}

#[async_trait]
impl Transport for ChannelTransport {
    async fn send(&self, message: Message) -> McpResult<()> {
        self.outgoing.send(message).map_err(|_| McpError::Closed)
    }

    async fn recv(&self) -> McpResult<Option<Message>> {
        Ok(self.incoming.lock().await.recv().await)
    }
}

/// A transport over a child process's stdin and stdout.
///
/// Framing is newline-delimited `JSON`, as MCP prescribes for stdio. The child is
/// killed on drop, and its stderr is drained to the `tracing` log so a chatty
/// server never blocks on a full pipe.
pub struct StdioTransport {
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    stdout: Mutex<BufReader<ChildStdout>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl StdioTransport {
    /// Spawns `program` with `args`, wiring its standard streams.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Io`] when the process cannot be spawned or one of its
    /// streams is unavailable.
    pub fn spawn(program: impl AsRef<OsStr>, args: &[String]) -> McpResult<Self> {
        let mut command = Command::new(program);
        command.args(args);
        Self::from_command(command)
    }

    /// Spawns a pre-configured command, overriding its standard streams.
    ///
    /// # Errors
    ///
    /// Returns [`McpError::Io`] when the process cannot be spawned or one of its
    /// streams is unavailable.
    pub fn from_command(mut command: Command) -> McpResult<Self> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdin".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("child has no stdout".to_owned()))?;
        let stderr_task = child.stderr.take().map(|stderr| {
            tokio::spawn(drain_stderr(stderr))
        });
        Ok(Self {
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(BufReader::new(stdout)),
            stderr_task,
        })
    }

    /// The operating-system process id of the child, when it is still running.
    #[must_use]
    pub fn id(&self) -> Option<u32> {
        self.child.try_lock().ok().and_then(|child| child.id())
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(&self, message: Message) -> McpResult<()> {
        let mut stdin = self.stdin.lock().await;
        write_framed(&mut *stdin, &message).await
    }

    async fn recv(&self) -> McpResult<Option<Message>> {
        let mut stdout = self.stdout.lock().await;
        read_framed(&mut *stdout).await
    }

    async fn close(&self) -> McpResult<()> {
        let mut child = self.child.lock().await;
        if let Err(error) = child.start_kill() {
            tracing::debug!(%error, "mcp child was already gone");
        }
        let _ = child.wait().await;
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

async fn drain_stderr(stderr: ChildStderr) {
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => tracing::debug!(target: "rustychain_mcp::stderr", "{}", line.trim_end()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jsonrpc::{Notification, Request, RequestId, Response};
    use serde_json::json;

    #[tokio::test]
    async fn framing_round_trips_over_a_byte_stream() {
        let (left, right) = tokio::io::duplex(512);
        let (left_read, mut left_write) = tokio::io::split(left);
        let (right_read, mut right_write) = tokio::io::split(right);
        let mut left_read = BufReader::new(left_read);
        let mut right_read = BufReader::new(right_read);

        let request = Message::Request(Request::new(
            RequestId::Number(1),
            "tools/list",
            Some(json!({ "cursor": "c" })),
        ));
        write_framed(&mut left_write, &request).await.expect("write");
        assert_eq!(read_framed(&mut right_read).await.expect("read"), Some(request));

        let response = Message::Response(Response::success(RequestId::Number(1), json!({})));
        write_framed(&mut right_write, &response).await.expect("write");
        assert_eq!(
            read_framed(&mut left_read).await.expect("read"),
            Some(response)
        );
    }

    #[tokio::test]
    async fn framing_skips_blank_lines_and_sees_end_of_stream() {
        let (mut writer, reader) = tokio::io::duplex(64);
        let (reader, _keep_open) = tokio::io::split(reader);
        let mut reader = BufReader::new(reader);
        writer
            .write_all(b"\n\n{\"jsonrpc\":\"2.0\",\"method\":\"ping\"}\n")
            .await
            .expect("write");
        let message = read_framed(&mut reader).await.expect("read");
        assert!(matches!(message, Some(Message::Notification(_))));

        drop(writer);
        assert_eq!(read_framed(&mut reader).await.expect("eof"), None);
    }

    #[tokio::test]
    async fn channel_pair_is_bidirectional() {
        let (left, right) = ChannelTransport::pair();
        let ping = Message::Notification(Notification::new("ping", None));
        left.send(ping.clone()).await.expect("send");
        assert_eq!(right.recv().await.expect("recv"), Some(ping.clone()));

        right.send(ping.clone()).await.expect("send");
        assert_eq!(left.recv().await.expect("recv"), Some(ping));
    }

    #[tokio::test]
    async fn a_dropped_channel_end_reports_closed() {
        let (left, right) = ChannelTransport::pair();
        drop(right);
        let error = left
            .send(Message::Notification(Notification::new("ping", None)))
            .await
            .expect_err("closed");
        assert!(matches!(error, McpError::Closed));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_round_trips_through_a_child_process() {
        let transport = StdioTransport::spawn("cat", &[]).expect("spawn cat");
        let request = Message::Request(Request::new(RequestId::Number(1), "ping", None));
        transport.send(request.clone()).await.expect("send");
        let echoed = transport.recv().await.expect("recv");
        assert_eq!(echoed, Some(request));
        transport.close().await.expect("close");
    }
}
