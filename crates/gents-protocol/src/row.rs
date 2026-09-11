//! Serde mirrors for replicated collection rows.
//!
//! These types are deliberately permissive: stable identity keys remain
//! required, while other nullable scalars are wrapped in `Option<T>` because
//! DefraDB may omit unpopulated fields from GraphQL responses. Collection/list
//! fields use a custom deserializer so both missing arrays and explicit `null`
//! values deserialize as empty vectors. Callers should treat these as the wire
//! shape, not a runtime invariant.

use serde::{Deserialize, Deserializer, Serialize};

use crate::request_lifecycle::RequestLifecycleState;

pub use crate::behavior_readiness::{
    decode_behavior_readiness_snapshot, effective_behavior_readiness_admission,
    project_behavior_readiness, project_behavior_readiness_source,
    project_behavior_readiness_summary, AgentBehaviorReadinessRow, BehaviorReadinessEntry,
    BehaviorReadinessProcessState, BehaviorReadinessProjection, BehaviorReadinessSnapshot,
    BehaviorReadinessSourceEntry, BehaviorReadinessState, BehaviorReadinessSummary,
    BehaviorReadinessUnavailableReason, BehaviorReadinessUnknownReason,
    EffectiveBehaviorReadinessAdmission, ProjectedBehaviorReadiness,
    ProjectedBehaviorReadinessSummary, BEHAVIOR_READINESS_FORMAT_VERSION,
};

pub(crate) fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRuntimeRow {
    pub agent_did: String,
    #[serde(default)]
    pub reconcile_phase: Option<String>,
    #[serde(default)]
    pub behavior_executor_capacity: Option<i64>,
    #[serde(default)]
    pub behavior_executor_queue_depth: Option<i64>,
    #[serde(default)]
    pub behavior_executor_status_json: Option<String>,
    #[serde(default)]
    pub last_reconcile_result: Option<String>,
    #[serde(default)]
    pub last_reconcile_error: Option<String>,
    #[serde(default)]
    pub last_reconcile_completed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentRequestRow {
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    pub request_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub admission_kind: Option<String>,
    #[serde(default)]
    pub admission_signer_did: Option<String>,
    #[serde(default)]
    pub admission_signature: Option<String>,
    #[serde(default)]
    pub enrollment_request_id: Option<String>,
    #[serde(default)]
    pub enrollment_request_digest: Option<String>,
    #[serde(default)]
    pub enrollment_admin_did: Option<String>,
    #[serde(default)]
    pub enrollment_authorization_sequence: Option<i64>,
    #[serde(default)]
    pub enrollment_authorization_expires_at: Option<String>,
    #[serde(default)]
    pub runtime_issuer_did: Option<String>,
    #[serde(default)]
    pub runtime_source_request_id: Option<String>,
    #[serde(default)]
    pub runtime_source_kind: Option<String>,
    #[serde(default)]
    pub runtime_bridge_author_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub retry_parent_request: Option<String>,
    #[serde(default)]
    pub retry_parent_request_doc_id: Option<String>,
    #[serde(default)]
    pub retry_root_request: Option<String>,
    #[serde(default)]
    pub retry_key: Option<String>,
    #[serde(default)]
    pub superseded_by_request: Option<String>,
    #[serde(default)]
    pub superseded_by_request_doc_id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    /// Execution-owner budget resolved from InferenceExecution, not a caller override.
    #[serde(default)]
    pub max_total_tokens: Option<i64>,
    #[serde(default)]
    pub input: Option<crate::request_input::RequestInput>,
    #[serde(default)]
    pub lifecycle_state: Option<RequestLifecycleState>,
    #[serde(default)]
    pub backend_id: Option<String>,
    #[serde(default)]
    pub execution_origin: Option<String>,
    #[serde(default)]
    pub caused_by_trigger_id: Option<String>,
    #[serde(default)]
    pub caused_by_trigger_kind: Option<String>,
    #[serde(default)]
    pub caused_by_correlation: Option<String>,
    #[serde(default)]
    pub caused_by_trigger_context: Option<String>,
    #[serde(default)]
    pub caused_by_source_doc_id: Option<String>,
    #[serde(default)]
    pub caused_by_trigger_doc_id: Option<String>,
    #[serde(default)]
    pub caused_by_parent_request_id: Option<String>,
    #[serde(default)]
    pub caused_by_parent_request_doc_id: Option<String>,
    #[serde(default)]
    pub caused_by_parent_tool_call_id: Option<String>,
    #[serde(default)]
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub failure_reason: Option<String>,
    #[serde(default)]
    pub terminalized_at: Option<String>,
    #[serde(default)]
    pub terminal_redrive_attempts: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<String>,
    #[serde(default)]
    pub execution_generation: Option<String>,
    #[serde(default)]
    pub execution_lease_expires_at: Option<String>,
    #[serde(default)]
    pub execution_progress_seq: Option<i64>,
    #[serde(default)]
    pub background_completion_input_through_sequence: Option<i64>,
    #[serde(default)]
    pub background_completion_notification_keys_json: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
    #[serde(default)]
    pub retry_count: Option<i64>,
    #[serde(default)]
    pub max_retries: Option<i64>,
    #[serde(default)]
    pub interrupt_requested_at: Option<String>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub subagent_depth: Option<i64>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Signed workspace reference scope, independent of the executing principal.
    #[serde(default)]
    pub workspace_owner_agent_did: Option<String>,
    #[serde(default)]
    pub workspace_authority: Option<String>,
    #[serde(default)]
    pub workspace_seal_hash: Option<String>,
}

impl AgentRequestRow {
    /// Terminal per `RequestLifecycleState::is_terminal`. A row with no
    /// (or an absent) `lifecycle_state` is not terminal.
    pub fn is_terminal(&self) -> bool {
        self.lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal)
    }

    /// Claimable per `RequestLifecycleState::is_claimable`.
    pub fn is_claimable(&self) -> bool {
        self.lifecycle_state
            .is_some_and(RequestLifecycleState::is_claimable)
    }
}

/// Replicated envelope for human attention. Clients render this row without
/// hydrating the referenced transcript or domain document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MailboxItemRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub item_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub status: String,
    pub kind: String,
    pub action: String,
    pub title: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub payload: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub graph_run_id: Option<String>,
    #[serde(default)]
    pub cause_doc_id: Option<String>,
    pub target_agent_did: String,
    pub target_behavior_id: String,
    #[serde(default)]
    pub expected_collection: Option<String>,
    #[serde(default)]
    pub parent_item_id: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub resolved_at: Option<String>,
    #[serde(default)]
    pub resolved_doc_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentResponseRow {
    pub response_key: String,
    #[serde(default)]
    pub request_doc_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub behavior_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub token_count: Option<i64>,
    #[serde(default)]
    pub progress_seq: Option<i64>,
    #[serde(default)]
    pub reasoning_progress_seq: Option<i64>,
    #[serde(default)]
    pub materialized_message_sequence: Option<i64>,
    #[serde(default)]
    pub materialized_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub interrupted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessageRow {
    pub message_key: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub sequence: Option<i64>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoalRow {
    pub goal_id: String,
    /// User-managed labels; do not affect goal continuation or token accounting.
    #[serde(
        default,
        deserialize_with = "deserialize_null_default",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub tags: Vec<String>,
    pub session_id: String,
    pub agent_did: String,
    #[serde(default)]
    pub creation_key: Option<String>,
    #[serde(default)]
    pub objective: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub token_budget: Option<i64>,
    #[serde(default)]
    pub tokens_used: Option<i64>,
    #[serde(default)]
    pub active_time_seconds: Option<i64>,
    #[serde(default)]
    pub active_started_at: Option<String>,
    #[serde(default)]
    pub consecutive_blocked_audits: Option<i64>,
    #[serde(default)]
    pub last_blocked_request_id: Option<String>,
    #[serde(default)]
    pub last_blocked_reason: Option<String>,
    #[serde(default)]
    pub last_continued_from_request_id: Option<String>,
    #[serde(default)]
    pub continuation_sequence: Option<i64>,
    #[serde(default)]
    pub wrapup_requested: Option<bool>,
    #[serde(default)]
    pub wrapup_completed: Option<bool>,
    #[serde(default)]
    pub infrastructure_retry_count: Option<i64>,
    #[serde(default)]
    pub last_failure: Option<String>,
    #[serde(default)]
    pub completion_evidence: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCallRow {
    pub tool_call_key: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub message_sequence: Option<i64>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_call_id: Option<String>,
    #[serde(default)]
    pub args: Option<String>,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub lifecycle_state: Option<String>,
    #[serde(default)]
    pub child_request_id: Option<String>,
    #[serde(default)]
    pub await_mode: Option<String>,
    #[serde(default)]
    pub cancel_policy: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub deadline_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub selected_service_id: Option<String>,
    #[serde(default)]
    pub selected_tool_name: Option<String>,
    #[serde(default)]
    pub tool_failure_class: Option<String>,
    #[serde(default)]
    pub denial_reason: Option<String>,
    #[serde(default)]
    pub denied_argv: Option<Vec<String>>,
    #[serde(default)]
    pub denied_command: Option<String>,
    #[serde(default)]
    pub denied_argument: Option<String>,
    #[serde(default)]
    pub denied_subcommand: Option<String>,
    #[serde(default)]
    pub denied_prefix: Option<Vec<String>>,
    #[serde(default)]
    pub policy_mode: Option<String>,
    #[serde(default)]
    pub policy_network: Option<String>,
    #[serde(default)]
    pub cancel_cause: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<i64>,
    #[serde(default)]
    pub partial_output_tail: Option<String>,
    #[serde(default)]
    pub partial_output_seq: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolResultRow {
    /// Exact spill document identity for observation/merge; not a content heuristic.
    #[serde(default, rename = "_docID", skip_serializing)]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub tool_name: Option<String>,
    #[serde(default)]
    pub tool_input: Option<String>,
    #[serde(default)]
    pub output_text: Option<String>,
    #[serde(default)]
    pub truncated: Option<bool>,
    #[serde(default)]
    pub truncation_metadata: Option<String>,
    #[serde(default)]
    pub tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub discarded_because_interrupted: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompactionEntryRow {
    pub compaction_key: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub requester_did: Option<String>,
    #[serde(default)]
    pub sequence: Option<i64>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files_read: Option<String>,
    #[serde(default)]
    pub files_modified: Option<String>,
    #[serde(default)]
    pub messages_compacted: Option<i64>,
    #[serde(default)]
    pub compacted_through_sequence: Option<i64>,
    #[serde(default)]
    pub original_tokens: Option<i64>,
    #[serde(default)]
    pub compacted_tokens: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentialRow {
    #[serde(default, rename = "_docID")]
    pub doc_id: Option<String>,
    pub credential_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub chatgpt_plan_type: Option<String>,
    #[serde(default)]
    pub is_fedramp: Option<bool>,
    #[serde(default)]
    pub access_token_expires_at: Option<String>,
    #[serde(default)]
    pub last_refresh: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceEntry {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceRegistryRow {
    pub service_id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub tailscale_ip: Option<String>,
    #[serde(default)]
    pub lan_ip: Option<String>,
    #[serde(default)]
    pub mcp_port: Option<i64>,
    #[serde(default)]
    pub mcp_path: Option<String>,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub send_agent_did: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tools: Vec<ToolServiceEntry>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Persisted snapshot of one MCP service's health, written by the agent's
/// `health_checker` on every probe cycle. `status` carries the raw
/// `ToolServiceHealthState` vocabulary ("healthy" / "degraded" / "evicted" /
/// "reconnecting"; see `tool_service_health::ToolServiceHealthState`) so the
/// operator UI can distinguish back-off from in-flight retry without going
/// through the collapsed three-state `ToolServiceHealthProjection`.
/// `failure_count` / `k_max` / `backoff_until` give the K-model context per
/// the design in `Proofs/MCPHealth/{State,Transition}.lean`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceHealthStateRow {
    pub service_id: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub tool_count: Option<i64>,
    #[serde(default)]
    pub failure_count: Option<i64>,
    #[serde(default)]
    pub k_max: Option<i64>,
    #[serde(default)]
    pub backoff_until: Option<String>,
    #[serde(default)]
    pub last_probe_at: Option<String>,
    #[serde(default)]
    pub last_seen: Option<String>,
    #[serde(default)]
    pub last_error_class: Option<String>,
    #[serde(default)]
    pub last_error_message: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_request_row_roundtrips() {
        let json = r#"{
            "_docID": "doc-1",
            "request_id": "req-1",
            "agent_did": "did:test:amy",
            "behavior_id": "amy-code",
            "session_id": "s-1",
            "retry_parent_request": "",
            "retry_root_request": "req-1",
            "superseded_by_request": "",
            "content": "hello",
            "max_total_tokens": 4096,
            "input": {
                "selected_skill_ids": ["review"],
                "cwd": "/workspace",
                "initial_title": {"text": "Review", "source": "task"},
                "goal_continuation": {"sequence": 1, "wrapup": false}
            },
            "lifecycle_state": "pending",
            "backend_id": "",
            "execution_origin": "interactive",
            "failure_reason": "",
            "created_at": "2026-04-13T12:00:00Z",
            "retry_count": 0,
            "max_retries": 3
        }"#;
        let row: AgentRequestRow = serde_json::from_str(json).expect("parse");
        assert_eq!(row.doc_id.as_deref(), Some("doc-1"));
        assert_eq!(row.request_id, "req-1");
        assert_eq!(row.retry_count, Some(0));
        assert_eq!(row.max_total_tokens, Some(4096));
        let input = row.input.as_ref().expect("typed input");
        assert_eq!(input.selected_skill_ids, ["review"]);
        assert_eq!(input.cwd.as_deref(), Some("/workspace"));
        assert_eq!(
            input.initial_title.as_ref().unwrap().source,
            crate::session::SessionTitleSource::Task
        );
        assert_eq!(input.goal_continuation.as_ref().unwrap().sequence, 1);
        assert!(!input.goal_continuation.as_ref().unwrap().wrapup);
        assert_eq!(row.lifecycle_state, Some(RequestLifecycleState::Pending));
        assert!(row.is_claimable());
        assert!(!row.is_terminal());
        let re: String = serde_json::to_string(&row).expect("serialize");
        assert!(!re.contains("_docID"));
        let round: AgentRequestRow = serde_json::from_str(&re).expect("reparse");
        assert_eq!(round.doc_id, None);
        assert_eq!(
            AgentRequestRow {
                doc_id: None,
                ..row
            },
            round
        );
    }

    #[test]
    fn request_row_nullable_input_and_claim_observations_are_independent() {
        for input in [
            serde_json::Value::Null,
            serde_json::json!({"selected_skill_ids": null}),
        ] {
            let row: AgentRequestRow = serde_json::from_value(serde_json::json!({
                "request_id": "request-1", "input": input,
                "backend_id": "resolved-at-claim", "max_total_tokens": 0
            }))
            .expect("nullable row decoder");
            assert_eq!(row.backend_id.as_deref(), Some("resolved-at-claim"));
            assert_eq!(
                row.max_total_tokens,
                Some(0),
                "pinned exhausted budget is not absent"
            );
            if let Some(input) = row.input {
                assert!(input.selected_skill_ids.is_empty());
            }
        }
        let row: AgentRequestRow = serde_json::from_value(serde_json::json!({
            "request_id": "request-1", "input": null, "backend_id": null, "max_total_tokens": null
        }))
        .unwrap();
        assert!(row.input.is_none());
        assert!(row.backend_id.is_none());
        assert!(row.max_total_tokens.is_none());
    }

    #[test]
    fn agent_request_row_missing_lifecycle_state_is_not_terminal_or_claimable() {
        let row: AgentRequestRow =
            serde_json::from_str(r#"{ "request_id": "req-2" }"#).expect("parse");
        assert_eq!(row.lifecycle_state, None);
        assert!(!row.is_claimable());
        assert!(!row.is_terminal());
    }

    #[test]
    fn agent_request_row_rejects_unknown_lifecycle_state_naming_it() {
        let json = r#"{ "request_id": "req-3", "lifecycle_state": "bogus" }"#;
        let err =
            serde_json::from_str::<AgentRequestRow>(json).expect_err("must reject unknown state");
        assert!(err.to_string().contains("bogus"), "{err}");
    }

    #[test]
    fn agent_request_row_terminal_states_report_terminal() {
        for state in RequestLifecycleState::ALL {
            let json = format!(
                r#"{{ "request_id": "req-4", "lifecycle_state": "{}" }}"#,
                state.as_str()
            );
            let row: AgentRequestRow = serde_json::from_str(&json).expect("parse");
            assert_eq!(row.is_terminal(), state.is_terminal(), "{state:?}");
            assert_eq!(row.is_claimable(), state.is_claimable(), "{state:?}");
        }
    }

    #[test]
    fn tool_service_registry_defaults_send_agent_did_to_false() {
        let json = r#"{
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "mcp_port": 9201,
            "mcp_path": "/mcp"
        }"#;
        let row: ToolServiceRegistryRow = serde_json::from_str(json).expect("parse");
        assert!(!row.send_agent_did);
    }

    #[test]
    fn tool_service_registry_treats_null_send_agent_did_as_false() {
        let json = r#"{
            "service_id": "observability-mcp",
            "hostname": "studio-1",
            "mcp_port": 9201,
            "mcp_path": "/mcp",
            "send_agent_did": null
        }"#;
        let row: ToolServiceRegistryRow = serde_json::from_str(json).expect("parse");
        assert!(!row.send_agent_did);
    }
}
