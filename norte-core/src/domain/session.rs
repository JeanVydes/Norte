use std::collections::{BTreeMap, VecDeque};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::Grant;
use crate::domain::message::Message;
use crate::id::{NodeId, SessionId};

/// Durable position of a session in the turn loop. Unknown phases fail to
/// deserialize instead of being coerced to a default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Phase {
    #[default]
    Idle,
    Streaming,
    AwaitingTool,
    AwaitingApproval,
}

impl Phase {
    #[must_use]
    pub fn is_idle(self) -> bool {
        matches!(self, Phase::Idle)
    }

    #[must_use]
    pub fn is_blocked(self) -> bool {
        matches!(self, Phase::AwaitingTool | Phase::AwaitingApproval)
    }
}

/// Durable conversation state: where the active branch is, what the engine is
/// waiting for, the concurrency token and the host-visible extras.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head: Option<NodeId>,
    pub phase: Phase,
    /// Incremented by exactly one per commit; the optimistic-concurrency token.
    pub version: u64,
    pub turn: u64,
    /// Steering messages, drained at the next safe point.
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub queue: VecDeque<Message>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Session {
    #[must_use]
    pub fn new() -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            head: None,
            phase: Phase::Idle,
            version: 0,
            turn: 0,
            queue: VecDeque::new(),
            grants: Vec::new(),
            metadata: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.phase.is_idle()
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_phase_fails_to_deserialize() {
        let result: Result<Phase, _> = serde_json::from_str("\"teleporting\"");
        assert!(result.is_err());
    }

    #[test]
    fn phases_round_trip() {
        for phase in [
            Phase::Idle,
            Phase::Streaming,
            Phase::AwaitingTool,
            Phase::AwaitingApproval,
        ] {
            let json = serde_json::to_string(&phase).expect("serialize");
            let decoded: Phase = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(decoded, phase);
        }
    }
}
