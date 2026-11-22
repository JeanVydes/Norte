use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use futures_util::SinkExt;
use norte_core::Error;
use serde_json::Value;
use tokio::sync::{Mutex, broadcast, oneshot};
use tokio_tungstenite::tungstenite::protocol::Message;

use crate::browser::error::CdpError;

pub(in crate::browser) type Pending =
    Arc<Mutex<BTreeMap<i64, oneshot::Sender<std::result::Result<Value, String>>>>>;

pub(in crate::browser) fn cdp_to_tool(error: CdpError) -> Error {
    error.into_tool_error()
}

/// Routes one CDP message by id or broadcasts it as an event.
pub(in crate::browser) async fn dispatch(
    message: &Value,
    pending: &Pending,
    events: &broadcast::Sender<Value>,
) {
    if let Some(id) = message.get("id").and_then(Value::as_i64) {
        let response = if let Some(error) = message.get("error") {
            Err(error.to_string())
        } else {
            Ok(message.get("result").cloned().unwrap_or(Value::Null))
        };
        if let Some(sender) = pending.lock().await.remove(&id) {
            let _ = sender.send(response);
        }
    }
    if message.get("method").is_some() {
        let _ = events.send(message.clone());
    }
}

/// Fails every command waiting on a socket that just went away.
pub(in crate::browser) async fn fail_pending(pending: &Pending, reason: &str) {
    let pending = std::mem::take(&mut *pending.lock().await);
    for (_, sender) in pending {
        let _ = sender.send(Err(reason.to_owned()));
    }
}

/// Dialogs pause page scripts until someone answers them. Accept normal
/// dialogs with the browser's suggested text; dismiss `beforeunload` so the
/// tool does not lose the page without an explicit navigation.
pub(in crate::browser) async fn answer_dialog<S>(
    sink: &mut S,
    ids: &AtomicI64,
    params: &Value,
) -> std::result::Result<(), String>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let kind = params["type"].as_str().unwrap_or("alert");
    let accept = kind != "beforeunload";
    let prompt_text = if accept && kind == "prompt" {
        params["defaultPrompt"].as_str().unwrap_or_default()
    } else {
        ""
    };
    let command = serde_json::json!({
        "id": ids.fetch_add(1, Ordering::Relaxed),
        "method": "Page.handleJavaScriptDialog",
        "params": { "accept": accept, "promptText": prompt_text }
    });
    sink.send(Message::Text(command.to_string().into()))
        .await
        .map_err(|error| error.to_string())
}
