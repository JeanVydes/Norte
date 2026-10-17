use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::capability::Capabilities;
use crate::domain::message::Message;
use crate::domain::part::ToolCallPart;
use crate::domain::session::Session;
use crate::id::{ModelRef, ToolName};

/// A typed view of the request a policy decides on. Policies perform no I/O:
/// anything external is fetched by a layer and placed in session metadata.
pub struct PolicyView<'a> {
    pub session: &'a Session,
    pub model: &'a ModelRef,
    pub capabilities: &'a Capabilities,
    pub call: &'a ToolCallPart,
    pub message: &'a Message,
    /// Decisions produced by earlier policies, in declared order.
    pub prior: &'a [PolicyDecision],
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decision {
    Allow,
    Deny(String),
    RequireApproval(String),
    Transform(Value),
    RateLimit(Duration),
}

/// A decision recorded together with the identity of the policy that made it,
/// so the same inputs keep producing the same auditable outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyDecision {
    pub policy: String,
    pub decision: Decision,
}

pub trait Policy: Send + Sync {
    fn name(&self) -> &str;

    fn decide(&self, view: &PolicyView<'_>) -> Decision;
}

/// Ordered, immutable policy list. Later policies see earlier decisions.
#[derive(Clone, Default)]
pub struct PolicySet {
    policies: Vec<Arc<dyn Policy>>,
}

impl PolicySet {
    #[must_use]
    pub fn new(policies: Vec<Arc<dyn Policy>>) -> Self {
        Self { policies }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PolicyOutcome {
    Allow,
    Deny {
        policy: String,
        reason: String,
    },
    RequireApproval {
        policy: String,
        reason: String,
    },
    Transform {
        policy: String,
        arguments: Value,
    },
    RateLimit {
        policy: String,
        retry_after: Duration,
    },
}

/// Folds the evaluated decisions with a fixed precedence: a denial always wins,
/// then approval, then argument rewriting, then rate limiting.
#[must_use]
pub fn combine(decisions: &[PolicyDecision]) -> PolicyOutcome {
    let mut outcome = PolicyOutcome::Allow;
    for PolicyDecision { policy, decision } in decisions {
        match decision {
            Decision::Deny(reason) => {
                return PolicyOutcome::Deny {
                    policy: policy.clone(),
                    reason: reason.clone(),
                };
            }
            Decision::RequireApproval(reason) => {
                outcome = PolicyOutcome::RequireApproval {
                    policy: policy.clone(),
                    reason: reason.clone(),
                };
            }
            Decision::Transform(arguments) => {
                outcome = PolicyOutcome::Transform {
                    policy: policy.clone(),
                    arguments: arguments.clone(),
                };
            }
            Decision::RateLimit(retry_after) => {
                if outcome == PolicyOutcome::Allow {
                    outcome = PolicyOutcome::RateLimit {
                        policy: policy.clone(),
                        retry_after: *retry_after,
                    };
                }
            }
            Decision::Allow => {}
        }
    }
    outcome
}

/// Convenience for the common "approve everything that names this tool" policy.
pub struct ToolApprovalPolicy {
    name: String,
    tools: Vec<ToolName>,
    reason: String,
}

impl ToolApprovalPolicy {
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
    use crate::id::{CallId, ModelRef};

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
}
