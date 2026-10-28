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

pub use crate::node_readiness::{
    decode_node_readiness_snapshot, effective_agent_readiness_admission,
    is_behavior_unavailable_rejection, project_node_readiness, project_node_readiness_source,
    project_node_readiness_summary, AgentReadinessEntry, AgentReadinessSourceEntry,
    AgentReadinessState, AgentReadinessUnavailableReason, AgentReadinessUnknownReason,
    EffectiveAgentReadinessAdmission, NodeReadinessProcessState, NodeReadinessProjection,
    NodeReadinessRow, NodeReadinessSnapshot, NodeReadinessSummary, ProjectedAgentReadiness,
    ProjectedNodeReadinessSummary, BEHAVIOR_NOT_ASSIGNED_MESSAGE, NODE_READINESS_FORMAT_VERSION,
};

pub(crate) fn deserialize_null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A stored key that is decoded and discarded; every value compares equal.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RetiredKey(serde::de::IgnoredAny);

impl PartialEq for RetiredKey {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl Eq for RetiredKey {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeRuntimeRow {
    pub node_did: String,
    #[serde(default)]
    pub reconcile_phase: Option<String>,
    #[serde(default)]
    pub agent_executor_capacity: Option<i64>,
    #[serde(default)]
    pub agent_executor_queue_depth: Option<i64>,
    #[serde(default)]
    pub agent_executor_status_json: Option<String>,
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
    pub purpose: Option<crate::request_admission::RequestPurpose>,
    #[serde(default)]
    pub node_did: Option<String>,
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
    pub agent_id: Option<String>,
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
    /// Exact terminal answer selection. Superseded work has no execution answer;
    /// absence on other terminal rows remains incomplete.
    #[serde(default)]
    pub terminal_output: Option<crate::output::TerminalOutput>,
    #[serde(default)]
    pub terminal_redrive_attempts: Option<i64>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub claimed_at: Option<String>,
    #[serde(default)]
    pub execution_generation: Option<String>,
    /// Lease length installed at claim. The request owner explicitly renews at
    /// bounded cadence; output never extends expiry or rewrites this row.
    #[serde(default)]
    pub execution_lease_secs: Option<i64>,
    /// Sole lease deadline. Renewal compares the current generation and observed
    /// deadline; expired owners cannot revive themselves with output or renewal.
    #[serde(default)]
    pub execution_lease_expires_at: Option<String>,
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
    /// Positive terminal-owner evidence that no provider-capable work began.
    /// Missing/false never licenses inference from absent provider rows.
    #[serde(default)]
    pub stopped_before_running: Option<bool>,
    #[serde(default)]
    pub valid_until: Option<String>,
    #[serde(default)]
    pub request_hop: Option<i64>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Signed workspace reference scope, independent of the executing node.
    #[serde(default)]
    pub workspace_owner_node_did: Option<String>,
    #[serde(default)]
    pub workspace_authority: Option<String>,
    #[serde(default)]
    pub workspace_seal_hash: Option<String>,
}

impl AgentRequestRow {
    /// Observes the lifecycle's output obligation without rewriting retained rows.
    /// Mirrors `CanonicalOutput.observeTerminalSelection`.
    pub fn observed_terminal_output(&self) -> Option<&crate::output::TerminalOutput> {
        static NO_MESSAGE: crate::output::TerminalOutput = crate::output::TerminalOutput::NoMessage;
        match (self.lifecycle_state, self.terminal_output.as_ref()) {
            (Some(RequestLifecycleState::Superseded), None) => Some(&NO_MESSAGE),
            (_, selection) => selection,
        }
    }

    pub fn is_terminal(&self) -> bool {
        self.lifecycle_state
            .is_some_and(RequestLifecycleState::is_terminal)
    }

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
    pub node_did: String,
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
    pub cause_doc_id: Option<String>,
    pub target_node_did: String,
    pub target_agent_id: String,
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

// AgentMessage has no row type: `output::TranscriptMessage` is the single
// canonical shape.

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
    pub node_did: String,
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
    /// Physical read-envelope identity. Canonical message blocks bind this,
    /// never the human-readable tool_call_key or provider-local call ID.
    #[serde(default, rename = "_docID")]
    pub doc_id: Option<String>,
    #[serde(default)]
    pub node_did: Option<String>,
    #[serde(default)]
    pub request_doc_id: Option<String>,
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
    pub status: Option<String>,
    #[serde(default)]
    pub lifecycle_state: Option<String>,
    /// Physical accepted meta-call that created a separate native background
    /// execution. Not a provider call ID; absent for directly requested tools.
    #[serde(default)]
    pub spawned_by_tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub plugin_parent_tool_call_doc_id: Option<String>,
    #[serde(default)]
    pub plugin_effect_ordinal: Option<u32>,
    #[serde(default)]
    pub terminal_output: Option<crate::output::PresentedPayload>,
    #[serde(default)]
    pub await_mode: Option<String>,
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
    pub plugin_execution_receipt: Option<crate::plugin::PluginExecutionReceipt>,
    #[serde(default)]
    pub job_pin: Option<crate::job::JobPin>,
    #[serde(default)]
    pub job_progress: Option<crate::job::JobProgress>,
    /// Host-only: the last step's execution evidence and validated resume
    /// block, never shown to the guest.
    #[serde(default)]
    pub job_step_state: Option<serde_json::Value>,
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
    pub stuck_since: Option<String>,
    #[serde(default)]
    pub completion_notification_delivered_at: Option<String>,
    #[serde(default)]
    pub latency_ms: Option<i64>,
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

#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredentialRow {
    #[serde(default, rename = "_docID")]
    pub doc_id: Option<String>,
    pub credential_id: String,
    #[serde(default)]
    pub node_did: Option<String>,
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
    #[serde(default)]
    pub account_ref: Option<String>,
    #[serde(default)]
    pub connected_at: Option<String>,
    #[serde(default)]
    pub provider_account_key: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
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
    pub send_node_did: bool,
    #[serde(default, deserialize_with = "deserialize_null_default")]
    pub tools: Vec<ToolServiceEntry>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

/// Persisted snapshot of one MCP service's health. `status` carries the raw
/// `ToolServiceHealthState` vocabulary so the operator UI can distinguish
/// back-off from in-flight retry without the collapsed three-state projection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolServiceHealthStateRow {
    pub service_id: String,
    #[serde(default)]
    pub node_did: Option<String>,
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
