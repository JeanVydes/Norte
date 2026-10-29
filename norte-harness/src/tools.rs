//! The execution of the tools a model asked for.
//!
//! A turn that ends with tool calls comes here: the calls are gathered from the
//! head message, each one runs, and the results are committed as one `Tool`
//! message.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::domain::commit::Commit;
use crate::domain::node::Node;
use crate::domain::session::{Phase, Session};
use crate::event::EngineEvent;
use norte_core::domain::message::Message;
use norte_core::domain::part::{ToolCallPart, ToolResultPart};
use norte_core::error::Result;
use norte_core::id::ToolName;
use norte_core::tool::ToolCallInfo;

use super::{
    CALL_CANCELLED_REASON, CallSite, Harness, PreparedCall, ToolTurn, execute_call,
    results_in_call_order,
};

impl Harness {
    /// Runs the calls of the head message and commits their results.
    pub(super) async fn run_tools(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        cancellation: &CancellationToken,
        early: &[ToolResultPart],
    ) -> Result<Session> {
        let Some(head) = session.head else {
            return Ok(session.clone());
        };
        let head_node = self.store.node(head).await?;
        let calls: Vec<ToolCallPart> = head_node
            .message
            .tool_calls()
            .into_iter()
            .cloned()
            .collect();
        if calls.is_empty() {
            return Ok(session.clone());
        }

        // Everything the stream already produced — a call whose arguments
        // completed early — seeded in call order, so the tool message lists the
        // calls the way the assistant message does.
        let mut results = results_in_call_order(&calls, &[early]);
        let turn = ToolTurn { session, sender };

        for call in &calls {
            if results.iter().any(|result| result.call_id == call.call_id) {
                continue;
            }
            if cancellation.is_cancelled() {
                break;
            }
            self.run_one_tool(&turn, call, &mut results, cancellation)
                .await?;
            if cancellation.is_cancelled() {
                break;
            }
        }

        // A cancellation can land before every call has run. The calls it left
        // unrun are answered with a cancelled error, so the committed tool
        // message lists a result for every call of the head — the one invariant
        // a replay across models and the store's history depend on.
        if cancellation.is_cancelled() {
            for call in &calls {
                if !results.iter().any(|result| result.call_id == call.call_id) {
                    results.push(ToolResultPart::error(call.call_id, CALL_CANCELLED_REASON));
                }
            }
        }
        let settled = results_in_call_order(&calls, &[results.as_slice()]);
        let node = Node::message(Message::tool(settled), Some(head));
        let node_id = node.id;
        let metadata = session.metadata.clone();
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: vec![node],
                head: Some(node_id),
                phase: Phase::Idle,
                turn: session.turn,
                metadata: Some(metadata),
            },
            sender,
        )
        .await
    }

    /// The host metadata for one execution, consulted immediately before the
    /// executor runs.
    pub(super) fn call_metadata(
        &self,
        session: &Session,
        call: &ToolCallPart,
        tool: &ToolName,
        arguments: &Value,
    ) -> BTreeMap<String, Value> {
        self.config
            .tool_metadata
            .as_ref()
            .map_or_else(BTreeMap::new, |provider| {
                provider.metadata(&ToolCallInfo {
                    session: session.id,
                    turn: session.turn,
                    call_id: call.call_id,
                    tool: tool.clone(),
                    arguments: arguments.clone(),
                })
            })
    }

    /// Runs one call.
    async fn run_one_tool(
        &self,
        turn: &ToolTurn<'_>,
        call: &ToolCallPart,
        results: &mut Vec<ToolResultPart>,
        cancellation: &CancellationToken,
    ) -> Result<()> {
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            results.push(ToolResultPart::error(
                call.call_id,
                format!("invalid tool name: {}", call.name),
            ));
            return Ok(());
        };

        let metadata = self.call_metadata(turn.session, call, &tool_name, &call.arguments);
        let result = execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            turn.sender.clone(),
            CallSite {
                session: turn.session.id,
                turn: turn.session.turn,
            },
            PreparedCall {
                call: call.clone(),
                tool: tool_name,
                arguments: call.arguments.clone(),
                metadata,
            },
            cancellation.clone(),
        )
        .await;
        results.push(result);
        Ok(())
    }
}
