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
mod task_title;
mod transition;

pub use manual::{write_manual_agent_request, write_manual_agent_request_with_conversation_title};
pub(crate) use materialize::write_pending_agent_request_with_lineage_and_conversation_title;
pub use task_title::task_run_conversation_title;

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

    #[cfg(test)]
    const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Superseded | Self::Dead | Self::Interrupted
        )
    }

    #[cfg(test)]
    const fn is_nonterminal(self) -> bool {
        !self.is_terminal()
    }

    const fn is_active_runtime(self) -> bool {
        matches!(self, Self::Pending | Self::Claimed | Self::Processing)
    }
}

fn lifecycle_state_graphql_list(
    states: impl IntoIterator<Item = PersistedLifecycleState>,
) -> String {
    let states = states
        .into_iter()
        .map(|state| format!(r#""{}""#, state.as_str()))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{states}]")
}

#[cfg(test)]
pub(crate) fn nonterminal_lifecycle_state_graphql_list() -> String {
    lifecycle_state_graphql_list(
        PersistedLifecycleState::ALL
            .iter()
            .copied()
            .filter(|state| state.is_nonterminal()),
    )
}

pub(crate) fn active_runtime_lifecycle_state_graphql_list() -> String {
    lifecycle_state_graphql_list(
        PersistedLifecycleState::ALL
            .iter()
            .copied()
            .filter(|state| state.is_active_runtime()),
    )
}

fn lifecycle_state_graphql_list_for(states: &[PersistedLifecycleState]) -> String {
    lifecycle_state_graphql_list(states.iter().copied())
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
    use crate::lean_vocab_test::{
        assert_lean_contract_vocabulary_matches, assert_state_machine_contract_is_complete,
        lean_state_machine_contract, LeanContractVocabulary,
    };

    #[test]
    fn rust_request_lifecycle_state_vocabulary_matches_lean_model() {
        let rust_states = PersistedLifecycleState::ALL
            .iter()
            .copied()
            .map(PersistedLifecycleState::as_str)
            .collect::<Vec<_>>();
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "RequestState",
            rust_source: "PersistedLifecycleState::ALL",
            rust_values: &rust_states,
        });
    }

    #[test]
    fn rust_execution_origin_vocabulary_matches_lean_model() {
        let rust_origins = vec![
            ExecutionOrigin::Interactive.as_str(),
            ExecutionOrigin::Scheduled.as_str(),
        ];
        assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
            domain: "ExecutionOrigin",
            rust_source: "ExecutionOrigin::{Interactive, Scheduled}",
            rust_values: &rust_origins,
        });
    }

    #[test]
    fn request_state_machine_contract_is_complete() {
        assert_state_machine_contract_is_complete("Request");
    }

    #[test]
    fn persisted_lifecycle_terminal_partition_matches_lean_contract() {
        let request_machine = lean_state_machine_contract("Request");
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
            request_machine
                .nonterminal_states
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            terminal,
            request_machine
                .terminal_states
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
        );
        assert!(PersistedLifecycleState::InputRequired.is_nonterminal());
        assert!(!PersistedLifecycleState::InputRequired.is_active_runtime());
        assert!(PersistedLifecycleState::Interrupted.is_terminal());
        let expected_nonterminal_graphql_list = format!(
            "[{}]",
            request_machine
                .nonterminal_states
                .iter()
                .map(|state| format!(r#""{state}""#))
                .collect::<Vec<_>>()
                .join(", ")
        );
        assert_eq!(
            nonterminal_lifecycle_state_graphql_list(),
            expected_nonterminal_graphql_list
        );
        assert_eq!(
            active_runtime_lifecycle_state_graphql_list(),
            r#"["pending", "claimed", "processing"]"#
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
