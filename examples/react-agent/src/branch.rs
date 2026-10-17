//! Fan-out: once the agent has gathered the catalogue, two specialist models
//! analyse one half of it each.

use std::error::Error;

use futures_util::StreamExt;
use rustychain_core::{Engine, Message};
use serde_json::Value;

use crate::render;

/// Streams one single-turn analysis and returns the final text.
///
/// # Errors
///
/// Propagates engine, store, render and serialization failures.
pub(crate) async fn analyse(
    title: &str,
    colour: &str,
    engine: &Engine,
    system: &str,
    data: &Value,
) -> Result<String, Box<dyn Error>> {
    let session = engine.create().await?;
    engine
        .append_history(session.id, vec![Message::system(system)])
        .await?;

    let prompt = format!(
        "Here is the data as JSON:\n{}",
        serde_json::to_string(data)?
    );

    render::section(title, colour);
    let mut stream = engine.send(session.id, Message::user(prompt));
    while let Some(event) = stream.next().await {
        render::event(&event?);
    }
    let outcome = stream.finish().await?;
    render::emit("\n");

    Ok(outcome
        .message
        .as_ref()
        .map(Message::text)
        .unwrap_or_default())
}
