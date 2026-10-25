pub use gents_desktop_core::inference::{AgentView, InferenceBackendView, NodeView};

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
    pub agent_executor_capacity: Option<i64>,
    pub agent_executor_queue_depth: Option<i64>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "snake_case")]
pub enum AgentUnavailableReasonView {
    AgentDisabled,
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
pub enum AgentReadinessUnknownReasonView {
    ReadinessMissing,
    ReadinessMalformed,
    ReadinessVersionUnsupported,
    ProcessNotReady,
    RouterGenerationStale,
    AgentNotAssigned,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentReadinessStatusView {
    Ready {
        agent_id: String,
    },
    Unavailable {
        agent_id: String,
        reason: AgentUnavailableReasonView,
    },
    Unknown {
        agent_id: String,
        reason: AgentReadinessUnknownReasonView,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NodeReadinessSourceView {
    Current,
    Unknown {
        reason: AgentReadinessUnknownReasonView,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct NodeReadinessView {
    pub source: NodeReadinessSourceView,
    pub active_generation: Option<u64>,
    pub router_generation: Option<u64>,
    pub updated_at: Option<String>,
    pub agents: Vec<AgentReadinessStatusView>,
}

impl Default for NodeReadinessView {
    fn default() -> Self {
        Self {
            source: NodeReadinessSourceView::Unknown {
                reason: AgentReadinessUnknownReasonView::ReadinessMissing,
            },
            active_generation: None,
            router_generation: None,
            updated_at: None,
            agents: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentEnvironmentView {
    pub agent_id: String,
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

pub use gents::document_config::{
    Agent, AgentContext, AgentTargetDocument, ChainKeyBindingDocument, CompactionConfig,
    DatastoreToolSurfaceDocument, EventSource, InferenceExecution, InferenceProfile,
    InferenceSampling, Node, Schedule, ToolServiceRegistry, Tools, Trigger,
};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub emit_outcome: bool,
    pub task_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub agent_id: Option<String>,
    pub prompt_template: Option<String>,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    pub hooks: Vec<gents::document_config::TaskHook>,
    pub enabled: Option<bool>,
    pub output_schema_ref: Option<String>,
    pub tags: Vec<String>,
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
    pub event_count: usize,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunSummaryView {
    pub request_id: String,
    pub request_doc_id: Option<String>,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
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
    pub node_did: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    pub source_directory: Option<String>,
    pub tool_refs: Vec<String>,
    pub display_name: Option<String>,
    pub interface_json: Option<String>,
    pub enabled: Option<bool>,
    pub created_at: Option<String>,
    pub tags: Vec<String>,
}

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
    pub started_by: Option<super::LinkedSessionView>,
    pub session_id: String,
    pub node_did: String,
    pub requester_did: Option<String>,
    pub latest_request_doc_id: Option<String>,
    pub closed_at: Option<String>,
    pub archived_at: Option<String>,
    pub archive_revision: String,
    pub tags: Vec<String>,
    pub provenance: Option<gents_protocol::session::SessionProvenance>,
    pub title: Option<String>,
    pub preview_text: Option<String>,
    pub status: Option<String>,
    pub agent_id: Option<String>,
    pub latest_request_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    pub trigger_id: Option<String>,
    pub trigger_kind: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
    pub turn_state: Option<String>,
    /// Absent when the bounded projection did not query transcript aggregates.
    pub message_count: Option<usize>,
    pub tool_call_count: Option<usize>,
    /// Why this client cannot read the session, when it cannot.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub unreadable_reason: Option<String>,
    /// Why a send from this client into the session would be refused, when it would.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub unsendable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MailboxItemView {
    pub item_id: String,
    pub item_key: String,
    pub requester_did: String,
    pub node_did: String,
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
    pub target_node_did: String,
    pub target_agent_id: String,
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
            node_did: row.node_did.clone(),
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
            target_node_did: row.target_node_did.clone(),
            target_agent_id: row.target_agent_id.clone(),
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
    pub node_did: String,
    pub addr: String,
    pub source: Option<String>,
    pub graphql: Option<String>,
    pub dial_succeeded: bool,
    pub chat_safe: bool,
    pub routes: Vec<ClientRouteStatusView>,
    #[serde(default)]
    pub pairing: Vec<PairingCollectionStatusView>,
    pub last_error: Option<String>,
    pub node: NodeView,
    pub node_config: Option<Node>,
    pub agent_configs: Vec<Agent>,
    pub runtime: Option<RuntimeView>,
    pub node_readiness: NodeReadinessView,
    pub agents: Vec<AgentView>,
    pub agent_environments: Vec<AgentEnvironmentView>,
    pub inference_backends: Vec<InferenceBackendView>,
    pub inference_profiles: Vec<InferenceProfile>,
    pub inference_sampling: Vec<InferenceSampling>,
    pub inference_execution: Vec<InferenceExecution>,
    pub contexts: Vec<AgentContext>,
    pub compactions: Vec<CompactionConfig>,
    pub tools: Vec<Tools>,
    pub tool_service_registries: Vec<ToolServiceRegistry>,
    pub agent_targets: Vec<AgentTargetDocument>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operator_config_authoritative_node_dids: Option<Vec<String>>,
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
    /// Presentation-only label; never enrollment authority or route selection.
    pub server_label: Option<String>,
    pub owner_node: String,
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
            owner_node: result.owner_node,
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
    pub node_did: String,
    pub source: Option<String>,
}
