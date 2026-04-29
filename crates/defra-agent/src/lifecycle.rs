use std::sync::Arc;

use anyhow::Result;
use defra_node::EmbeddedNode;

use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session;
use crate::watcher::AgentRequest;

mod claim;
mod lookup;
pub mod manual;
mod materialize;
mod query;
mod recovery;
mod rows;
mod transition;

pub use manual::write_manual_agent_request;
pub(crate) use materialize::write_pending_agent_request_with_lineage;

pub const DEFAULT_REQUEST_MAX_RETRIES: u32 = 3;

fn graphql_retry_root_request(retry_root_request: Option<&str>, request_id: &str) -> String {
    escape_graphql_string(retry_root_request.unwrap_or(request_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalLifecycleState {
    Pending,
    Claimed,
    Streaming,
    Completed,
    Failed,
    Superseded,
    Interrupted,
    Dead,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    Claimed,
    Superseded,
    Interrupted,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionOrigin {
    Interactive,
    Scheduled,
}

impl ExecutionOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Scheduled => "scheduled",
        }
    }

    pub(crate) fn from_persisted(value: Option<&str>) -> Self {
        match value {
            Some("scheduled") => Self::Scheduled,
            _ => Self::Interactive,
        }
    }
}

/// Lineage describing which trigger (if any) caused a request to be
/// materialized. Both fields are `None` for interactive user submissions and
/// for recovery paths; scheduled and event-driven triggers populate them so
/// downstream readers can follow the causal chain.
#[derive(Debug, Clone, Default)]
pub struct TriggerLineage {
    pub trigger_id: Option<String>,
    pub trigger_kind: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum PersistedLifecycleState {
    Pending,
    Claimed,
    Processing,
    InputRequired,
    Completed,
    Failed,
    Superseded,
    Dead,
    Interrupted,
}

impl PersistedLifecycleState {
    const ALL: [Self; 9] = [
        Self::Pending,
        Self::Claimed,
        Self::Processing,
        Self::InputRequired,
        Self::Completed,
        Self::Failed,
        Self::Superseded,
        Self::Dead,
        Self::Interrupted,
    ];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Claimed => "claimed",
            Self::Processing => "processing",
            Self::InputRequired => "inputRequired",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Superseded => "superseded",
            Self::Dead => "dead",
            Self::Interrupted => "interrupted",
        }
    }

    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Superseded | Self::Dead | Self::Interrupted
        )
    }

    const fn is_nonterminal(self) -> bool {
        !self.is_terminal()
    }
}

pub(crate) fn nonterminal_lifecycle_state_graphql_list() -> String {
    let states = PersistedLifecycleState::ALL
        .iter()
        .copied()
        .filter(|state| state.is_nonterminal())
        .map(|state| format!(r#""{}""#, state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{states}]")
}

pub struct RequestLifecycle {
    node: Arc<EmbeddedNode>,
    agent_name: String,
    agent_did: String,
    behavior_id: String,
    execution_origin: ExecutionOrigin,
    backend_id: String,
    failure_reason: Option<String>,
    request: AgentRequest,
    response_doc_id: Option<String>,
    progress_seq: u32,
    deadline_duration_secs: u64,
    claimed_deadline_at: Option<chrono::DateTime<chrono::Utc>>,
    state: LocalLifecycleState,
    valid_until_at_claim: Option<chrono::DateTime<chrono::Utc>>,
}

impl RequestLifecycle {
    pub(crate) fn claimed_deadline_at(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.claimed_deadline_at
    }

    /// Test-only accessor for S8 caching validation. Do not call from production code.
    pub fn valid_until_at_claim_for_test(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.valid_until_at_claim
    }
}

#[derive(Debug, Default)]
pub struct RecoveryReport {
    pub requests_recovered: usize,
    pub responses_recovered: usize,
    pub conversations_recovered: usize,
}

fn resolve_behavior_id(default_behavior_id: &str, requested_behavior_id: Option<&str>) -> String {
    requested_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_behavior_id)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_lifecycle_terminal_partition_matches_trigger_bridge() {
        let nonterminal = PersistedLifecycleState::ALL
            .iter()
            .copied()
            .filter(|state| state.is_nonterminal())
            .map(|state| state.as_str())
            .collect::<Vec<_>>();
        let terminal = PersistedLifecycleState::ALL
            .iter()
            .copied()
            .filter(|state| state.is_terminal())
            .map(|state| state.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            nonterminal,
            vec!["pending", "claimed", "processing", "inputRequired"]
        );
        assert_eq!(
            terminal,
            vec!["completed", "failed", "superseded", "dead", "interrupted"]
        );
        assert!(PersistedLifecycleState::InputRequired.is_nonterminal());
        assert!(PersistedLifecycleState::Interrupted.is_terminal());
        assert_eq!(
            nonterminal_lifecycle_state_graphql_list(),
            r#"["pending", "claimed", "processing", "inputRequired"]"#
        );
        assert_eq!(
            ExecutionOrigin::from_persisted(Some("scheduled")),
            ExecutionOrigin::Scheduled
        );
        assert_eq!(
            ExecutionOrigin::from_persisted(Some("interactive")),
            ExecutionOrigin::Interactive
        );
        assert_eq!(
            ExecutionOrigin::from_persisted(None),
            ExecutionOrigin::Interactive
        );
    }
}
