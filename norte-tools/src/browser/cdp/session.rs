use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use norte_core::Result;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, mpsc, oneshot};
use tokio::time::timeout;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::protocol::{Message, WebSocketConfig};

use super::protocol::{Pending, answer_dialog, cdp_to_tool, dispatch, fail_pending};
use super::targets::Tab;
use crate::browser::error::{BrowserErrorCode, CdpError, tool_fail_code};
use crate::browser::limits::BrowserLimits;

/// A CDP connection shared by all actions that drive one tab.
///
/// One reader task owns the socket. Commands are sent through a channel and
/// their replies are routed by id; events go to subscribers. This keeps a slow
/// command from consuming the event another caller is waiting for, and lets a
/// dialog be answered even while a page script is blocked on it.
#[derive(Clone)]
pub(in crate::browser) struct CdpSession {
    endpoint: String,
    target_id: String,
    outgoing: mpsc::UnboundedSender<Message>,
    pending: Pending,
    events: broadcast::Sender<Value>,
    next_id: Arc<AtomicI64>,
    closed: Arc<AtomicBool>,
    reader: Arc<tokio::task::JoinHandle<()>>,
}

impl Drop for CdpSession {
    fn drop(&mut self) {
        if Arc::strong_count(&self.reader) == 1 {
            self.reader.abort();
        }
    }
}

impl CdpSession {
    pub(in crate::browser) async fn connect(
        limits: &BrowserLimits,
        tab: &Tab,
        timeout_dur: Duration,
    ) -> Result<Self> {
        let mut config = WebSocketConfig::default();
        config.max_message_size = Some(4 * 1024 * 1024);
        config.max_frame_size = Some(4 * 1024 * 1024);
        let (socket, _) = connect_async_with_config(&tab.ws_url, Some(config), false)
            .await
            .map_err(CdpError::from)
            .map_err(cdp_to_tool)?;
        let (mut sink, mut stream) = socket.split();
        let (outgoing, mut incoming) = mpsc::unbounded_channel::<Message>();
        let pending: Pending = Arc::new(Mutex::new(BTreeMap::new()));
        let (events, _) = broadcast::channel(256);
        let next_id = Arc::new(AtomicI64::new(1));
        let reader_pending = Arc::clone(&pending);
        let reader_events = events.clone();
        let reader_ids = Arc::clone(&next_id);
        let reader_closed = Arc::new(AtomicBool::new(false));
        let task_closed = Arc::clone(&reader_closed);

        let reader = tokio::spawn(async move {
            let mut reason = "the CDP socket closed".to_owned();
            loop {
                tokio::select! {
                    message = incoming.recv() => {
                        match message {
                            Some(message) => {
                                if sink.send(message).await.is_err() {
                                    "the CDP socket rejected a command".clone_into(&mut reason);
                                    break;
                                }
                            }
                            None => break,
                        }
                    }
                    message = stream.next() => {
                        match message {
                            Some(Ok(Message::Text(text))) => {
                                let event: Value = match serde_json::from_str(&text) {
                                    Ok(event) => event,
                                    Err(_) => continue,
                                };
                                if event.get("method").and_then(Value::as_str)
                                    == Some("Page.javascriptDialogOpening")
                                {
                                    let dialog = answer_dialog(
                                        &mut sink,
                                        &reader_ids,
                                        &event["params"],
                                    )
                                    .await;
                                    if let Err(error) = dialog {
                                        reason = error;
                                        break;
                                    }
                                }
                                dispatch(&event, &reader_pending, &reader_events).await;
                            }
                            Some(Ok(_)) => {}
                            Some(Err(error)) => {
                                reason = error.to_string();
                                break;
                            }
                            None => break,
                        }
                    }
                }
            }
            task_closed.store(true, Ordering::Release);
            fail_pending(&reader_pending, &reason).await;
        });

        let session = Self {
            endpoint: limits.endpoint.trim_end_matches('/').to_owned(),
            target_id: tab.id.clone(),
            outgoing,
            pending,
            events,
            next_id,
            closed: reader_closed,
            reader: Arc::new(reader),
        };
        session.send("Page.enable", json!({}), timeout_dur).await?;
        session
            .send("Runtime.enable", json!({}), timeout_dur)
            .await?;
        Ok(session)
    }

    pub(in crate::browser) fn target_id(&self) -> &str {
        &self.target_id
    }

    pub(in crate::browser) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(in crate::browser) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire) || self.outgoing.is_closed()
    }

    pub(in crate::browser) fn subscribe(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }

    pub(in crate::browser) async fn send(
        &self,
        method: &'static str,
        params: Value,
        timeout_dur: Duration,
    ) -> Result<Value> {
        self.send_result(method, params, timeout_dur)
            .await
            .map_err(cdp_to_tool)
    }

    pub(in crate::browser) async fn evaluate(
        &self,
        expression: &str,
        timeout_dur: Duration,
    ) -> Result<Value> {
        let response = self
            .send_result(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "returnByValue": true,
                    "awaitPromise": true
                }),
                timeout_dur,
            )
            .await
            .map_err(cdp_to_tool)?;
        if let Some(details) = response.get("exceptionDetails") {
            let description = details["exception"]["description"]
                .as_str()
                .or_else(|| details["text"].as_str())
                .unwrap_or("JavaScript threw while evaluating the page script");
            return Err(tool_fail_code(
                format!("page script failed: {description}"),
                BrowserErrorCode::PageScript,
            ));
        }
        if response["result"]["subtype"].as_str() == Some("error") {
            return Err(tool_fail_code(
                "page script returned an error value",
                BrowserErrorCode::PageScript,
            ));
        }
        Ok(response["result"]["value"].clone())
    }

    async fn send_result(
        &self,
        method: &'static str,
        params: Value,
        timeout_dur: Duration,
    ) -> std::result::Result<Value, CdpError> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);
        let request = json!({ "id": id, "method": method, "params": params });
        if self
            .outgoing
            .send(Message::Text(request.to_string().into()))
            .is_err()
        {
            self.pending.lock().await.remove(&id);
            return Err(CdpError::Closed);
        }
        match timeout(timeout_dur, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(CdpError::Protocol { method, message }),
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(CdpError::Timeout(format!(
                    "CDP command {method} timed out after {timeout_dur:?}"
                )))
            }
        }
    }
}

/// Waits for one event after the caller has subscribed, so events emitted
/// between a command and the wait cannot be lost.
pub(in crate::browser) async fn wait_for_event<F>(
    events: &mut broadcast::Receiver<Value>,
    method: &'static str,
    matches: F,
    timeout_dur: Duration,
) -> Result<()>
where
    F: Fn(&Value) -> bool,
{
    let wait = async {
        loop {
            match events.recv().await {
                Ok(event)
                    if event.get("method").and_then(Value::as_str) == Some(method)
                        && matches(&event["params"]) =>
                {
                    return Ok(());
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(CdpError::Closed);
                }
            }
        }
    };
    timeout(timeout_dur, wait)
        .await
        .unwrap_or_else(|_| {
            Err(CdpError::Timeout(format!(
                "timed out waiting for CDP event {method}"
            )))
        })
        .map_err(cdp_to_tool)
}
