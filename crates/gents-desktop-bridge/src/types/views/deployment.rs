use serde::Serialize;
use ts_rs::TS;

use gents_desktop_core::client::PeerMutationResult;
use gents_protocol::row::MailboxItemRow;

use super::bootstrap::DesktopBootstrapSummary;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct P2PHealthView {
    pub status: String,
    pub connected_peer_count: usize,
    pub replicator_count: usize,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub last_ok_at: Option<String>,
    pub last_failure_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncHealthView {
    pub state: String,
    pub last_error: Option<String>,
    pub connected_peer_count: usize,
    pub pending_dag_count: Option<usize>,
    pub persisted_pending_dag_count: Option<usize>,
    pub push_retry_marker_count: Option<usize>,
    pub exhausted_fetch_count: Option<u64>,
    pub quarantined_dag_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct PairingCollectionStatusView {
    pub collection_id: String,
    pub pairing_retry_count: u32,
    pub last_retry_at: Option<String>,
    pub last_retry_error_class: Option<String>,
    pub stuck_since: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeView {
    pub reconcile_phase: Option<String>,
    pub last_reconcile_result: Option<String>,
    pub last_reconcile_error: Option<String>,
    pub updated_at: Option<String>,
    pub behavior_executor_capacity: Option<i64>,
    pub behavior_executor_queue_depth: Option<i64>,
}

#[cfg(test)]
mod runtime_view_tests {
    use super::*;

    #[test]
    fn diagnostic_runtime_dto_cannot_serialize_readiness_authority() {
        let value = serde_json::to_value(RuntimeView {
            reconcile_phase: Some("idle".to_string()),
            last_reconcile_result: Some("applied".to_string()),
            last_reconcile_error: None,
            updated_at: Some("2026-08-29T00:00:00Z".to_string()),
            behavior_executor_capacity: Some(1),
            behavior_executor_queue_depth: Some(0),
        })
        .expect("serialize diagnostic runtime view");
        let object = value.as_object().expect("runtime view object");
        for forbidden in [
            "processState",
            "activeGeneration",
            "routerGeneration",
            "defaultBehaviorId",
            "runnableBehaviorCount",
            "unavailableBehaviorCount",
        ] {
            assert!(
                !object.contains_key(forbidden),
                "forbidden field: {forbidden}"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorUnavailableReasonView {
    BehaviorDisabled,
    RuntimeConfigurationInvalid,
    BackendNotConfigured,
    BackendDisabled,
    BackendTemporarilyUnavailable,
    CredentialsRequired,
    InferenceProfileInvalid,
    ToolConfigurationInvalid,
    ToolSurfaceUnavailable,
    ExecutorStartFailed,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum BehaviorReadinessUnknownReasonView {
    ReadinessMissing,
    ReadinessMalformed,
    ReadinessVersionUnsupported,
    ReadinessStale,
    ProcessNotReady,
    RouterGenerationStale,
    BehaviorNotAssigned,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum BehaviorReadinessStatusView {
    Ready {
        behavior_id: String,
    },
    Unavailable {
        behavior_id: String,
        reason: BehaviorUnavailableReasonView,
    },
    Unknown {
        behavior_id: String,
        reason: BehaviorReadinessUnknownReasonView,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BehaviorReadinessSourceView {
    Current,
    Unknown {
        reason: BehaviorReadinessUnknownReasonView,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorReadinessView {
    pub source: BehaviorReadinessSourceView,
    pub active_generation: Option<u64>,
    pub router_generation: Option<u64>,
    pub updated_at: Option<String>,
    pub behaviors: Vec<BehaviorReadinessStatusView>,
}

impl Default for BehaviorReadinessView {
    fn default() -> Self {
        Self {
            source: BehaviorReadinessSourceView::Unknown {
                reason: BehaviorReadinessUnknownReasonView::ReadinessMissing,
            },
            active_generation: None,
            router_generation: None,
            updated_at: None,
            behaviors: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentPrincipalView {
    pub agent_did: String,
    pub display_name: Option<String>,
    pub default_behavior_id: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorView {
    pub behavior_id: String,
    pub agent_did: String,
    pub display_name: String,
    pub description: Option<String>,
    pub context_id: Option<String>,
    pub inference_profile_id: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub tags: Vec<String>,
    pub created_at: Option<String>,
}

/// Resolved, presentation-safe description of a configured behavior environment.
///
/// AgentBehavior stores references to shared configuration documents. Clients
/// should not have to repeat those joins (or infer tool semantics), so the
/// bridge materializes the environment once alongside the raw configuration
/// projection.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorEnvironmentView {
    pub behavior_id: String,
    pub display_name: String,
    pub enabled: bool,
    pub is_default: bool,
    pub model_name: Option<String>,
    pub inference_profile_name: Option<String>,
    pub workspace_root: Option<String>,
    pub file_access: String,
    pub bash_access: String,
    pub network_access: Option<String>,
    pub skill_names: Vec<String>,
    pub session_count: usize,
    pub active_session_count: usize,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InferenceBackendView {
    pub backend_id: String,
    pub name: Option<String>,
    pub provider_kind: Option<String>,
    pub openai_wire_api: Option<String>,
    pub endpoint: Option<String>,
    pub auth_kind: Option<String>,
    pub connect_timeout_secs: Option<i64>,
    pub discovery_timeout_secs: Option<i64>,
    pub api_key_configured: bool,
    pub api_key_env_var: Option<String>,
    pub max_concurrent: Option<i64>,
    pub max_queue_depth: Option<i64>,
    pub enabled: Option<bool>,
    pub models: Vec<String>,
    pub probe_status: Option<String>,
}

// Configurations without credentials use their canonical serialized documents.
// Derived presentation belongs to BehaviorEnvironmentView, not another config shape.
pub use gents::document_config::{
    AgentBehavior as AgentBehaviorDocument, AgentContext, AgentPrincipal, ChainKeyBindingDocument,
    CompactionConfig, DatastoreToolSurfaceDocument, EventSource, InferenceExecution,
    InferenceProfile, InferenceSampling, Schedule, SubagentTargetDocument, ToolServiceRegistry,
    Tools, Trigger,
};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub task_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub behavior_id: Option<String>,
    pub prompt_template: Option<String>,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    pub enabled: Option<bool>,
    pub recent_runs: TaskRecentRunsView,
    pub run_history: Vec<TaskRunSummaryView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRecentRunsView {
    pub total_fires: u64,
    pub last_attempt_at: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub schedule_count: usize,
    pub event_trigger_count: usize,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunSummaryView {
    pub request_id: String,
    pub request_doc_id: Option<String>,
    pub agent_did: String,
    pub requester_did: Option<String>,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub execution_origin: Option<String>,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SkillView {
    pub skill_id: String,
    pub agent_did: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub tool_refs: Vec<String>,
    pub display_name: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
}

/// Authored trigger and its observed delivery state share one read-only envelope.
/// Reusable schedules and event sources contain no execution counters.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TriggerView {
    pub config: Trigger,
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_fired_source_doc_id: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub session_id: String,
    pub agent_did: String,
    pub requester_did: Option<String>,
    pub latest_request_doc_id: Option<String>,
    pub closed_at: Option<String>,
    pub tags: Vec<String>,
    pub provenance: Option<gents_protocol::session::SessionProvenance>,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub behavior_id: Option<String>,
    pub latest_request_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    pub trigger_id: Option<String>,
    pub trigger_kind: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub turn_state: Option<String>,
    /// Absent when the bounded deployment projection did not query transcript
    /// aggregates. Never inferred from resident transcript rows.
    pub message_count: Option<usize>,
    pub tool_call_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MailboxItemView {
    pub item_id: String,
    pub item_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub status: String,
    pub kind: String,
    pub action: String,
    pub title: String,
    pub summary: Option<String>,
    pub payload: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub graph_run_id: Option<String>,
    pub cause_doc_id: Option<String>,
    pub target_agent_did: String,
    pub target_behavior_id: String,
    pub expected_collection: Option<String>,
    pub parent_item_id: Option<String>,
    pub deadline_at: Option<String>,
    pub created_at: String,
}

impl From<&MailboxItemRow> for MailboxItemView {
    fn from(row: &MailboxItemRow) -> Self {
        Self {
            item_id: row.doc_id.clone(),
            item_key: row.item_key.clone(),
            requester_did: row.requester_did.clone(),
            agent_did: row.agent_did.clone(),
            status: row.status.clone(),
            kind: row.kind.clone(),
            action: row.action.clone(),
            title: row.title.clone(),
            summary: row.summary.clone(),
            payload: row.payload.clone(),
            source_kind: row.source_kind.clone(),
            source_id: row.source_id.clone(),
            session_id: row.session_id.clone(),
            request_id: row.request_id.clone(),
            graph_run_id: row.graph_run_id.clone(),
            cause_doc_id: row.cause_doc_id.clone(),
            target_agent_did: row.target_agent_did.clone(),
            target_behavior_id: row.target_behavior_id.clone(),
            expected_collection: row.expected_collection.clone(),
            parent_item_id: row.parent_item_id.clone(),
            deadline_at: row.deadline_at.clone(),
            created_at: row.created_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientRouteStatusView {
    pub route_id: String,
    pub direction: String,
    pub directory_id: String,
    pub transport_peer_id: Option<String>,
    pub address: Option<String>,
    pub template: Option<String>,
    pub desired: bool,
    pub applied: bool,
    pub live_match: bool,
    pub filter_summary: String,
    pub last_error: Option<String>,
    pub retry_count: u32,
    pub last_retry_at: Option<String>,
    pub last_retry_error_class: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentView {
    pub peer_id: String,
    pub label: String,
    pub agent_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
    pub dial_succeeded: bool,
    pub chat_safe: bool,
    pub routes: Vec<ClientRouteStatusView>,
    #[serde(default)]
    pub pairing: Vec<PairingCollectionStatusView>,
    pub last_error: Option<String>,
    pub agent_principal: AgentPrincipalView,
    pub principal_config: Option<AgentPrincipal>,
    pub behavior_configs: Vec<AgentBehaviorDocument>,
    pub runtime: Option<RuntimeView>,
    pub behavior_readiness: BehaviorReadinessView,
    pub behaviors: Vec<BehaviorView>,
    pub behavior_environments: Vec<BehaviorEnvironmentView>,
    pub inference_backends: Vec<InferenceBackendView>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_execution: Vec<InferenceExecution>,
    pub contexts: Vec<AgentContext>,
    pub compactions: Vec<CompactionConfig>,
    pub tools: Vec<Tools>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub subagent_targets: Vec<SubagentTargetDocument>,
    pub datastore_tool_surfaces: Vec<DatastoreToolSurfaceDocument>,
    pub chain_key_bindings: Vec<ChainKeyBindingDocument>,
    pub skills: Vec<SkillView>,
    pub tasks: Vec<TaskView>,
    pub schedules: Vec<Schedule>,
    pub event_sources: Vec<EventSource>,
    pub triggers: Vec<TriggerView>,
    pub sessions: Vec<SessionSummary>,
    pub mailbox_items: Vec<MailboxItemView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRuntimeSnapshot {
    pub local_peer_id: String,
    pub listen_addresses: Vec<String>,
    pub p2p_health: P2PHealthView,
    pub sync_health: Option<SyncHealthView>,
    pub enrollment_requests: Option<Vec<EnrollmentRequestView>>,
    pub bootstrap_errors: Vec<String>,
    pub last_mutation_error: Option<String>,
    pub focused_request_id: Option<String>,
    pub configured_peer_count: usize,
    pub dialed_peer_count: usize,
    pub peer_issue_count: usize,
    pub row_count: usize,
    pub approx_serialized_bytes: usize,
    pub deployments: Vec<DeploymentView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopClientSnapshot {
    pub bootstrap: DesktopBootstrapSummary,
    pub client: Option<DesktopRuntimeSnapshot>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerMutationView {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub connected: bool,
    pub warning: Option<String>,
}

impl From<PeerMutationResult> for PeerMutationView {
    fn from(result: PeerMutationResult) -> Self {
        Self {
            peer_id: result.peer_id,
            label: result.label,
            addr: result.addr,
            connected: result.connected,
            warning: result.warning,
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PeerRemoveResponse {
    #[serde(flatten)]
    pub snapshot: DesktopClientSnapshot,
    pub mutation: PeerMutationView,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentRequestView {
    pub request_id: String,
    pub network_id: String,
    pub admin_did: String,
    pub server_peer: String,
    /// Presentation-only label advertised by the authenticated status endpoint.
    /// It is never used for enrollment authority or route selection.
    pub server_label: Option<String>,
    pub owner_agent: String,
    pub state: String,
    pub expires_at: String,
}

impl From<gents_desktop_core::client::EnrollmentRequestResult> for EnrollmentRequestView {
    fn from(result: gents_desktop_core::client::EnrollmentRequestResult) -> Self {
        Self {
            request_id: result.request_id,
            network_id: result.network_id,
            admin_did: result.admin_did,
            server_peer: result.server_peer,
            server_label: None,
            owner_agent: result.owner_agent,
            state: result.state,
            expires_at: result.expires_at,
        }
    }
}

impl PeerRemoveResponse {
    pub fn new(snapshot: DesktopClientSnapshot, mutation: PeerMutationResult) -> Self {
        Self {
            snapshot,
            mutation: mutation.into(),
        }
    }
}

#[cfg(test)]
mod peer_remove_response_tests {
    use super::*;

    #[test]
    fn response_preserves_snapshot_shape_and_surfaces_mutation_result() {
        let snapshot = DesktopClientSnapshot {
            bootstrap: DesktopBootstrapSummary {
                default_agent_home: "/agent".to_string(),
                init_agent_name: None,
                init_agent_did: None,
                init_tool_ceiling: None,
                init_tool_root: None,
                desktop_home: "/desktop".to_string(),
                peer_directory_path: "/desktop/peers.json".to_string(),
                node_data_dir: "/desktop/node".to_string(),
                log_file_path: "/desktop/desktop.log".to_string(),
                agent_home_exists: true,
                desktop_home_exists: true,
                peer_directory_exists: true,
                client_state_exists: true,
                saved_peers: Vec::new(),
            },
            client: None,
        };
        let response = PeerRemoveResponse::new(
            snapshot,
            PeerMutationResult {
                peer_id: "peer-1".to_string(),
                label: "Workshop".to_string(),
                addr: "iroh://peer-1".to_string(),
                connected: false,
                warning: Some("partial cleanup".to_string()),
            },
        );

        let value = serde_json::to_value(response).expect("serialize peer remove response");
        assert_eq!(value["bootstrap"]["desktopHome"], "/desktop");
        assert!(value["client"].is_null());
        assert_eq!(value["mutation"]["peerId"], "peer-1");
        assert_eq!(value["mutation"]["warning"], "partial cleanup");
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NetworkStatusView {
    pub local_peer_id: Option<String>,
    pub local_peer_id_error: Option<String>,
    pub listen_addresses: Vec<String>,
    pub listen_addresses_error: Option<String>,
    pub connected_peers: Vec<String>,
    pub connected_peers_error: Option<String>,
    pub replicators: Vec<NetworkReplicatorView>,
    pub replicators_error: Option<String>,
    pub saved_peers: Vec<NetworkSavedPeerView>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NetworkReplicatorView {
    pub peer_id: Option<String>,
    pub address: Option<String>,
    pub collections: Vec<String>,
    pub status: Option<u8>,
    pub last_status_change: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct NetworkSavedPeerView {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub agent_did: String,
    pub source: Option<String>,
}
