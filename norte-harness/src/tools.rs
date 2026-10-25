//! The execution of the tools a model asked for.
//!
//! A turn that ends with tool calls comes here: the calls are gathered from the
//! head message, the policies decide whether each one may run, the approval is
//! resolved when a call is gated, and the results are committed as one
//! `Tool` message.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalRecord, ApprovalStatus};
use crate::domain::commit::Commit;
use crate::domain::node::Node;
use crate::domain::session::{Phase, Session};
use crate::event::EngineEvent;
use crate::policy::{PolicyOutcome, PolicyView, combine};
use norte_core::domain::message::Message;
use norte_core::domain::part::{ToolCallPart, ToolResultPart};
use norte_core::error::{Error, Result};
use norte_core::id::ToolName;
use norte_core::model::PARTIAL_TOOLS_KEY;
use norte_core::tool::ToolCallInfo;

use super::{
    Authorization, CallSite, Harness, PreparedCall, ToolTurn, denial_reason, execute_call,
    partial_results, request_messages, results_in_call_order,
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

        let capabilities = self.model.capabilities();
        let history = {
            let full = self.store.history(Some(head)).await?;
            request_messages(&full).to_vec()
        };
        // Everything that already produced a result — during this stream and
        // before a park — seeded in call order, so a policy reads it the way the
        // message lists the calls (FR-POL-4).
        let persisted = partial_results(session);
        let mut results = results_in_call_order(&calls, &[early, persisted.as_slice()]);
        let turn = ToolTurn {
            session,
            sender,
            message: &head_node.message,
            capabilities,
            history: &history,
        };

        for call in &calls {
            if results.iter().any(|result| result.call_id == call.call_id) {
                continue;
            }
            if cancellation.is_cancelled() {
                return Ok(session.clone());
            }
            if let Some(parked) = self
                .run_one_tool(&turn, call, &mut results, cancellation)
                .await?
            {
                return Ok(parked);
            }
            if cancellation.is_cancelled() {
                return Ok(session.clone());
            }
        }

        let settled = results_in_call_order(&calls, &[results.as_slice()]);
        let node = Node::message(Message::tool(settled), Some(head));
        let node_id = node.id;
        let mut metadata = session.metadata.clone();
        metadata.remove(PARTIAL_TOOLS_KEY);
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: vec![node],
                head: Some(node_id),
                phase: Phase::Idle,
                turn: session.turn,
                grants: session.grants.clone(),
                metadata: Some(metadata),
            },
            sender,
        )
        .await
    }

    /// The host metadata for one execution, consulted immediately before the
    /// executor runs so a resumed park is filled like an inline call (FR-TOOL-7).
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

    /// Runs one call, unless a policy or an approval says otherwise.
    #[allow(clippy::too_many_lines)] // one linear gate: policy, approval, run, record.
    async fn run_one_tool(
        &self,
        turn: &ToolTurn<'_>,
        call: &ToolCallPart,
        results: &mut Vec<ToolResultPart>,
        cancellation: &CancellationToken,
    ) -> Result<Option<Session>> {
        let ToolTurn {
            session,
            sender,
            message,
            capabilities,
            history,
        } = *turn;
        let Ok(tool_name) = ToolName::new(call.name.clone()) else {
            results.push(ToolResultPart::error(
                call.call_id,
                format!("invalid tool name: {}", call.name),
            ));
            return Ok(None);
        };

        let decisions = {
            let view = PolicyView {
                session,
                model: self.model.reference(),
                capabilities,
                call,
                message,
                prior: &[],
                results: results.as_slice(),
                history,
            };
            self.policies.evaluate(&view)
        };
        // `PolicyOutcome` is `#[non_exhaustive]`: the fallback below is
        // unreachable while this build knows every variant, and stays so that a
        // variant added later is refused rather than silently run.
        #[allow(unreachable_patterns)]
        let arguments = match combine(&decisions) {
            PolicyOutcome::Deny { reason, .. } => {
                results.push(ToolResultPart::error(call.call_id, reason.clone()));
                self.finish_call(session, sender, call, &tool_name, true, 0)
                    .await;
                return Ok(None);
            }
            PolicyOutcome::RateLimit { retry_after, .. } => {
                results.push(ToolResultPart::error(
                    call.call_id,
                    format!("rate limited; retry after {retry_after:?}"),
                ));
                self.finish_call(session, sender, call, &tool_name, true, 0)
                    .await;
                return Ok(None);
            }
            PolicyOutcome::Transform { arguments, .. } => arguments,
            PolicyOutcome::RequireApproval {
                arguments: rewritten,
                ..
            } => {
                let arguments = rewritten.unwrap_or_else(|| call.arguments.clone());
                match self
                    .authorize(session, call, &tool_name, sender, &arguments)
                    .await?
                {
                    Authorization::Granted => arguments,
                    Authorization::Pending => {
                        return Ok(Some(
                            self.await_approval(session, sender, results.as_slice())
                                .await?,
                        ));
                    }
                    Authorization::Denied(reason) => {
                        // The operator said no: the model gets that back as a
                        // failed call so it can adapt, exactly like a policy
                        // denial or a tool that errored.
                        results.push(ToolResultPart::error(call.call_id, reason));
                        self.finish_call(session, sender, call, &tool_name, true, 0)
                            .await;
                        return Ok(None);
                    }
                }
            }
            PolicyOutcome::Allow => call.arguments.clone(),
            // `PolicyOutcome` is `#[non_exhaustive]`: a variant added to the
            // core after this build cannot be honoured here, and running the
            // call anyway would silently ignore a policy the host configured.
            // Refusing is the only reading that cannot be wrong.
            other => {
                results.push(ToolResultPart::error(
                    call.call_id,
                    format!("policy outcome this harness does not understand: {other:?}"),
                ));
                self.finish_call(session, sender, call, &tool_name, true, 0)
                    .await;
                return Ok(None);
            }
        };
        let metadata = self.call_metadata(session, call, &tool_name, &arguments);
        let result = execute_call(
            Arc::clone(&self.tools),
            Arc::clone(&self.observers),
            sender.clone(),
            CallSite {
                session: session.id,
                turn: session.turn,
            },
            PreparedCall {
                call: call.clone(),
                tool: tool_name,
                arguments,
                metadata,
            },
            cancellation.clone(),
        )
        .await;
        results.push(result);
        Ok(None)
    }

    /// Reports that a call is over.
    async fn finish_call(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        call: &ToolCallPart,
        tool: &ToolName,
        is_error: bool,
        duration_ms: u64,
    ) {
        self.emit(
            sender,
            EngineEvent::ToolCallFinished {
                session: session.id,
                turn: session.turn,
                call_id: call.call_id,
                tool: tool.clone(),
                is_error,
                duration_ms,
            },
        )
        .await;
    }

    /// Resolves the approval state of one gated call.
    ///
    /// A call with no record gets one here, and the host is told. This is what
    /// makes a parked conversation resume: the request is rebuilt from the head
    /// message's tool calls, with a fresh record and a fresh id, rather than
    /// from a ledger that may never have travelled.
    async fn authorize(
        &self,
        session: &Session,
        call: &ToolCallPart,
        tool: &ToolName,
        sender: &mpsc::Sender<EngineEvent>,
        arguments: &Value,
    ) -> Result<Authorization> {
        let records = self.store.approvals(session.id).await?;
        let decided = records.iter().find(|record| record.call_id == call.call_id);
        if let Some(record) = decided {
            if record.is_expired_at(Utc::now()) || record.status == ApprovalStatus::Expired {
                return Err(Error::ApprovalExpired(record.id));
            }
            if record.status == ApprovalStatus::Denied {
                return Ok(Authorization::Denied(denial_reason(record)));
            }
            match record.authorize(Utc::now(), arguments) {
                Ok(()) => return Ok(Authorization::Granted),
                Err(Error::ApprovalDenied(reason)) if record.status == ApprovalStatus::Granted => {
                    // A grant that was revoked, or arguments that no longer
                    // match the digest it was issued for (FR-APPR-4).
                    return Ok(Authorization::Denied(reason));
                }
                Err(Error::ApprovalDenied(_)) => {}
                Err(error) => return Err(error),
            }
        }

        // A standing grant, scoped to the tool, the session or everything,
        // authorizes later calls without asking again (FR-APPR-3). A grant
        // scoped to one call is already covered by the record above.
        let standing = session
            .grants
            .iter()
            .any(|grant| grant.applies_to(session.id, call.call_id, tool));
        if standing {
            return Ok(Authorization::Granted);
        }

        if decided.is_some() {
            return Ok(Authorization::Pending);
        }

        let record = ApprovalRecord::new(
            session.id,
            call.call_id,
            tool.clone(),
            arguments,
            chrono::Duration::from_std(self.config.approval_ttl)
                .unwrap_or_else(|_| chrono::Duration::hours(1)),
        );
        self.store.save_approval(record.clone()).await?;
        self.emit(
            sender,
            EngineEvent::ApprovalRequested {
                session: session.id,
                approval: record.id,
                call_id: record.call_id,
                tool: record.tool.clone(),
            },
        )
        .await;
        Ok(Authorization::Pending)
    }

    /// Parks the turn until the host decides about a gated call.
    async fn await_approval(
        &self,
        session: &Session,
        sender: &mpsc::Sender<EngineEvent>,
        partial: &[ToolResultPart],
    ) -> Result<Session> {
        let mut metadata = session.metadata.clone();
        metadata.remove(PARTIAL_TOOLS_KEY);
        if !partial.is_empty() {
            if let Ok(value) = serde_json::to_value(partial) {
                metadata.insert(PARTIAL_TOOLS_KEY.to_owned(), value);
            }
        }
        self.commit(
            Commit {
                session_id: session.id,
                expected_version: Some(session.version),
                nodes: Vec::new(),
                head: None,
                phase: Phase::AwaitingApproval,
                turn: session.turn,
                grants: session.grants.clone(),
                metadata: Some(metadata),
            },
            sender,
        )
        .await
    }
}
