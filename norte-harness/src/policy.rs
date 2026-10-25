use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::domain::session::Session;
use norte_core::capability::Capabilities;
use norte_core::domain::message::Message;
use norte_core::domain::part::{ToolCallPart, ToolResultPart};
use norte_core::id::{ModelRef, ToolName};

/// A typed view of the request a policy decides on. Policies perform no I/O:
/// anything external is fetched by a layer and placed in session metadata.
pub struct PolicyView<'a> {
    /// The session the call belongs to.
    pub session: &'a Session,
    /// The model the turn is running.
    pub model: &'a ModelRef,
    /// What the model declared it can do.
    pub capabilities: &'a Capabilities,
    /// The call being decided.
    pub call: &'a ToolCallPart,
    /// The assistant message that carries the call.
    pub message: &'a Message,
    /// Decisions produced by earlier policies, in declared order.
    pub prior: &'a [PolicyDecision],
    /// Results the same assistant message already produced, in call order, so a
    /// policy can decide on what this request just did rather than only on what
    /// it asks. This is a sequential-path concept (FR-POL-4): a policy
    /// evaluated for a call dispatched early (FR-STREAM-4) always sees it empty,
    /// because none of the message's calls has run yet when they are started in
    /// parallel; a call the policy defers is evaluated later, on the normal
    /// path, and then sees whatever already ran. A policy that depends on it
    /// must leave `norte_harness::HarnessConfig::early_tool_execution` off.
    pub results: &'a [ToolResultPart],
    /// The messages of the request in flight: from the last user message to the
    /// assistant message carrying this call, inclusive, including the tool
    /// results committed so far. A parked request that resumes still shows the
    /// same slice; early dispatch sees what is committed up to the previous
    /// tool turn, because the message in flight is not committed yet.
    pub history: &'a [Message],
}

/// What a policy decided.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    /// Let the call run.
    Allow,
    /// Refuse the call, with a reason the model reads.
    Deny(String),
    /// Park the call for a human decision, with a prompt.
    RequireApproval(String),
    /// Run the call with rewritten arguments.
    Transform(Value),
    /// Refuse for now, asking to retry after the duration.
    RateLimit(Duration),
}

/// A decision recorded together with the identity of the policy that made it,
/// so the same inputs keep producing the same auditable outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDecision {
    /// The name of the policy that decided.
    pub policy: String,
    /// What it decided.
    pub decision: Decision,
}

/// A pure decision rule over one tool call.
pub trait Policy: Send + Sync {
    /// The policy's name, recorded with every decision.
    fn name(&self) -> &str;

    /// Decides what happens to the call.
    fn decide(&self, view: &PolicyView<'_>) -> Decision;
}

/// Ordered, immutable policy list. Later policies see earlier decisions.
#[derive(Clone, Default)]
pub struct PolicySet {
    policies: Vec<Arc<dyn Policy>>,
}

impl PolicySet {
    /// A set that evaluates the policies in order.
    #[must_use]
    pub fn new(policies: Vec<Arc<dyn Policy>>) -> Self {
        Self { policies }
    }

    /// Whether the set has no policies.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

    /// Evaluates every policy in order, each seeing the decisions before it.
    #[must_use]
    pub fn evaluate(&self, view: &PolicyView<'_>) -> Vec<PolicyDecision> {
        let mut decisions = Vec::with_capacity(self.policies.len());
        for policy in &self.policies {
            let scoped = PolicyView {
                session: view.session,
                model: view.model,
                capabilities: view.capabilities,
                call: view.call,
                message: view.message,
                prior: &decisions,
                results: view.results,
                history: view.history,
            };
            let decision = policy.decide(&scoped);
            decisions.push(PolicyDecision {
                policy: policy.name().to_owned(),
                decision,
            });
        }
        decisions
    }
}

/// The folded outcome of a set of decisions.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyOutcome {
    /// The call runs.
    Allow,
    /// The call is refused.
    Deny {
        /// The policy that denied it.
        policy: String,
        /// The reason, as the model reads it.
        reason: String,
    },
    /// The call is parked for a human decision.
    RequireApproval {
        /// The policy that parked it.
        policy: String,
        /// The prompt the human reads.
        reason: String,
        /// The arguments to run once approved, when an earlier policy rewrote
        /// them. `None` means the model's original arguments.
        arguments: Option<Value>,
    },
    /// The call runs with rewritten arguments.
    Transform {
        /// The policy that rewrote them.
        policy: String,
        /// The rewritten arguments.
        arguments: Value,
    },
    /// The call is refused for now.
    RateLimit {
        /// The policy that set the limit.
        policy: String,
        /// How long to wait before retrying.
        retry_after: Duration,
    },
}

/// Folds the evaluated decisions with a fixed precedence: a denial always wins,
/// then approval, then argument rewriting, then rate limiting.
///
/// The precedence does not depend on the order the policies are registered in.
/// In particular, a later `Transform` must not erase an earlier
/// `RequireApproval`: doing so would run a call a human was supposed to gate.
/// When both apply, the approval carries the last rewritten arguments, so the
/// human approves exactly what will run.
#[must_use]
pub fn combine(decisions: &[PolicyDecision]) -> PolicyOutcome {
    let mut approval: Option<(String, String)> = None;
    let mut transform: Option<(String, Value)> = None;
    let mut rate_limit: Option<(String, Duration)> = None;
    for PolicyDecision { policy, decision } in decisions {
        match decision {
            Decision::Deny(reason) => {
                return PolicyOutcome::Deny {
                    policy: policy.clone(),
                    reason: reason.clone(),
                };
            }
            Decision::RequireApproval(reason) => {
                approval = Some((policy.clone(), reason.clone()));
            }
            Decision::Transform(arguments) => {
                transform = Some((policy.clone(), arguments.clone()));
            }
            Decision::RateLimit(retry_after) => {
                if rate_limit.is_none() {
                    rate_limit = Some((policy.clone(), *retry_after));
                }
            }
            Decision::Allow => {}
        }
    }
    if let Some((policy, reason)) = approval {
        return PolicyOutcome::RequireApproval {
            policy,
            reason,
            arguments: transform.map(|(_, arguments)| arguments),
        };
    }
    if let Some((policy, arguments)) = transform {
        return PolicyOutcome::Transform { policy, arguments };
    }
    if let Some((policy, retry_after)) = rate_limit {
        return PolicyOutcome::RateLimit {
            policy,
            retry_after,
        };
    }
    PolicyOutcome::Allow
}

/// Convenience for the common "approve everything that names this tool" policy.
pub struct ToolApprovalPolicy {
    name: String,
    tools: Vec<ToolName>,
    reason: String,
}

impl ToolApprovalPolicy {
    /// A policy that parks any call to one of `tools`, with `reason`.
    #[must_use]
    pub fn new(name: impl Into<String>, tools: Vec<ToolName>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            tools,
            reason: reason.into(),
        }
    }
}

impl Policy for ToolApprovalPolicy {
    fn name(&self) -> &str {
        &self.name
    }

    fn decide(&self, view: &PolicyView<'_>) -> Decision {
        match ToolName::new(view.call.name.clone()) {
            Ok(tool) if self.tools.contains(&tool) => {
                Decision::RequireApproval(self.reason.clone())
            }
            _ => Decision::Allow,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use norte_core::id::{CallId, ModelRef};

    struct Deny;

    impl Policy for Deny {
        fn name(&self) -> &'static str {
            "deny"
        }

        fn decide(&self, _view: &PolicyView<'_>) -> Decision {
            Decision::Deny("never".to_owned())
        }
    }

    struct Later;

    impl Policy for Later {
        fn name(&self) -> &'static str {
            "later"
        }

        fn decide(&self, view: &PolicyView<'_>) -> Decision {
            if view.prior.is_empty() {
                Decision::Allow
            } else {
                Decision::RequireApproval("saw prior".to_owned())
            }
        }
    }

    struct Fixture {
        session: Session,
        model: ModelRef,
        capabilities: Capabilities,
        call: ToolCallPart,
        message: Message,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                session: Session::new(),
                model: ModelRef::from_str("openai/gpt-5").expect("model"),
                capabilities: Capabilities::default(),
                call: ToolCallPart {
                    call_id: CallId::new(),
                    name: "read_file".to_owned(),
                    arguments: Value::Null,
                    continuity: None,
                },
                message: Message::assistant("x"),
            }
        }

        fn view(&self) -> PolicyView<'_> {
            PolicyView {
                session: &self.session,
                model: &self.model,
                capabilities: &self.capabilities,
                call: &self.call,
                message: &self.message,
                prior: &[],
                results: &[],
                history: &[],
            }
        }
    }

    #[test]
    fn later_policies_see_earlier_decisions() {
        let set = PolicySet::new(vec![Arc::new(Later), Arc::new(Later)]);
        let fixture = Fixture::new();
        let decisions = set.evaluate(&fixture.view());
        assert_eq!(decisions.len(), 2);
        assert!(matches!(
            decisions[1].decision,
            Decision::RequireApproval(_)
        ));
    }

    #[test]
    fn a_deny_policy_short_circuits_the_set() {
        let set = PolicySet::new(vec![Arc::new(Deny), Arc::new(Later)]);
        let fixture = Fixture::new();
        assert!(matches!(
            combine(&set.evaluate(&fixture.view())),
            PolicyOutcome::Deny { .. }
        ));
    }

    #[test]
    fn denial_wins_over_everything() {
        let decisions = vec![
            PolicyDecision {
                policy: "approval".to_owned(),
                decision: Decision::RequireApproval("x".to_owned()),
            },
            PolicyDecision {
                policy: "deny".to_owned(),
                decision: Decision::Deny("no".to_owned()),
            },
        ];
        assert!(matches!(combine(&decisions), PolicyOutcome::Deny { .. }));
    }

    #[test]
    fn approval_wins_over_a_later_transform() {
        let rewritten = serde_json::json!({"path": "/workspace/rewritten"});
        let decisions = vec![
            PolicyDecision {
                policy: "approval".to_owned(),
                decision: Decision::RequireApproval("ask".to_owned()),
            },
            PolicyDecision {
                policy: "workspace".to_owned(),
                decision: Decision::Transform(rewritten.clone()),
            },
        ];
        match combine(&decisions) {
            PolicyOutcome::RequireApproval {
                policy,
                reason,
                arguments,
            } => {
                assert_eq!(policy, "approval");
                assert_eq!(reason, "ask");
                assert_eq!(arguments, Some(rewritten));
            }
            other => panic!("approval must survive a later transform, got {other:?}"),
        }
    }

    #[test]
    fn approval_carries_an_earlier_transform() {
        let rewritten = serde_json::json!({"path": "/workspace/rewritten"});
        let decisions = vec![
            PolicyDecision {
                policy: "workspace".to_owned(),
                decision: Decision::Transform(rewritten.clone()),
            },
            PolicyDecision {
                policy: "approval".to_owned(),
                decision: Decision::RequireApproval("ask".to_owned()),
            },
        ];
        match combine(&decisions) {
            PolicyOutcome::RequireApproval {
                policy, arguments, ..
            } => {
                assert_eq!(policy, "approval");
                assert_eq!(arguments, Some(rewritten));
            }
            other => panic!("approval must carry the rewritten arguments, got {other:?}"),
        }
    }
}
