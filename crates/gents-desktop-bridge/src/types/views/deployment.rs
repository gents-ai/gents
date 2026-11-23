pub use gents_observe::client_inference::{AgentView, InferenceBackendView, NodeView};

use serde::Serialize;
use ts_rs::TS;

use gents_protocol::row::MailboxItemRow;

use super::bootstrap::DesktopBootstrapSummary;

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(rename_all = "camelCase")]
pub struct SyncHealthView {
    pub state: String,
    pub cause: Option<String>,
    pub next_action: Option<String>,
    pub runtime_count: usize,
    pub connected_runtime_count: usize,
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
    SubscriptionNotEntitled,
    InferenceProfileInvalid,
    ToolConfigurationInvalid,
    ToolSurfaceUnavailable,
    /// The Agent's host tools read a published WorkspaceRoot it holds no
    /// `reader` on, so its tool surface cannot be admitted.
    WorkspaceRootReaderMissing,
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
    StorageUnavailable,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq, TS)]
#[serde(
    tag = "state",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentReadinessStatusView {
    Ready {
        agent_did: String,
    },
    Unavailable {
        agent_did: String,
        reason: AgentUnavailableReasonView,
    },
    Unknown {
        agent_did: String,
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
    pub agent_did: String,
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

pub use gents_documents::{
    Agent, AgentContext, AgentTargetDocument, CompactionConfig, DatastoreToolSurfaceDocument,
    EventSource, InferenceExecution, InferenceProfile, InferenceSampling, Node, Schedule,
    ToolServiceRegistry, Tools, Trigger,
};

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskView {
    pub emit_outcome: bool,
    pub task_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub agent_did: Option<String>,
    pub prompt_template: Option<String>,
    pub goal_objective_template: Option<String>,
    pub goal_token_budget: Option<i64>,
    pub goal_blocked_audit_threshold: Option<i64>,
    pub goal_max_infrastructure_retries: Option<i64>,
    pub hooks: Vec<gents_documents::TaskHook>,
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
    pub agent_did: Option<String>,
    pub lifecycle_state: Option<gents_protocol::request_lifecycle::RequestLifecycleState>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskOriginView {
    pub node_did: String,
    pub task_id: String,
    pub task_name: Option<String>,
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
    /// The first visible line of the session's latest assistant reply, as its
    /// node recorded it.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub reply_preview: Option<String>,
    pub status: Option<String>,
    pub agent_did: Option<String>,
    pub latest_request_id: Option<String>,
    pub task_id: Option<String>,
    pub task_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub task_origin: Option<TaskOriginView>,
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
    /// The folder the session's latest request ran with, as its node recorded
    /// it: a path on that node's computer, absent when the request named none.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    #[ts(optional = nullable)]
    pub cwd: Option<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub question_hash: Option<String>,
    pub source_kind: String,
    pub source_id: String,
    pub session_id: Option<String>,
    pub request_id: Option<String>,
    pub cause_doc_id: Option<String>,
    pub target_node_did: String,
    pub target_agent_did: String,
    pub expected_collection: Option<String>,
    pub parent_item_id: Option<String>,
    pub deadline_at: Option<String>,
    pub created_at: String,
}

/// An exact native item read; absence from the open mailbox is not closure.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct MailboxItemObservation {
    pub item: MailboxItemView,
    pub terminal: bool,
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
            question_hash: row
                .payload
                .as_deref()
                .filter(|payload| {
                    row.kind == "ask"
                        && row.action == "start_request"
                        && gents_protocol::mailbox_question::question_requires_reply_receipt(
                            payload,
                        )
                })
                .and_then(|payload| {
                    gents_protocol::mailbox_question::question_source_hash(
                        &row.title,
                        row.summary.as_deref(),
                        payload,
                        row.request_id.as_deref(),
                    )
                    .ok()
                }),
            source_kind: row.source_kind.clone(),
            source_id: row.source_id.clone(),
            session_id: row.session_id.clone(),
            request_id: row.request_id.clone(),
            cause_doc_id: row.cause_doc_id.clone(),
            target_node_did: row.target_node_did.clone(),
            target_agent_did: row.target_agent_did.clone(),
            expected_collection: row.expected_collection.clone(),
            parent_item_id: row.parent_item_id.clone(),
            deadline_at: row.deadline_at.clone(),
            created_at: row.created_at.clone(),
        }
    }
}

/// Why the native chat write fence refuses this deployment, with the next
/// action that reopens it (`ClientChatReadiness.Cause` and `nextAction`).
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatRefusalView {
    pub cause: String,
    pub next_action: String,
    pub detail: String,
}

impl From<gents_protocol::runtime_connection::ConnectionRefusalCause> for ChatRefusalView {
    fn from(cause: gents_protocol::runtime_connection::ConnectionRefusalCause) -> Self {
        Self {
            cause: cause.as_str().to_owned(),
            next_action: "connect_runtime".to_owned(),
            detail: "Connect this runtime to continue.".to_owned(),
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DeploymentView {
    pub local: bool,
    pub label: String,
    pub node_did: String,
    pub graphql: Option<String>,
    pub runtime_connection: Option<String>,
    pub chat_safe: bool,
    pub chat_refusal: Option<ChatRefusalView>,
    pub operator_access: bool,
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
    pub client_did: String,
    pub sync_health: SyncHealthView,
    pub access_requests: Vec<ClientAccessRequestView>,
    pub row_count: usize,
    pub approx_serialized_bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub operator_config_authoritative_node_dids: Option<Vec<String>>,
    pub deployments: Vec<DeploymentView>,
    pub unkept_changes: Vec<UnkeptChangeView>,
}

/// A change a home did not keep; `message` is what the person reads.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UnkeptChangeView {
    pub node_did: String,
    pub label: String,
    pub at: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DesktopClientSnapshot {
    pub bootstrap: DesktopBootstrapSummary,
    pub client: Option<DesktopRuntimeSnapshot>,
    /// What this client start set aside before opening, for the repair banner.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub repair_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRemoveResponse {
    #[serde(flatten)]
    pub snapshot: DesktopClientSnapshot,
    pub removed: bool,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientAccessRequestView {
    pub request_id: String,
    pub node_did: String,
    pub client_did: String,
    pub state: String,
}

impl From<gents_protocol::client_access::ClientAccessReceipt> for ClientAccessRequestView {
    fn from(receipt: gents_protocol::client_access::ClientAccessReceipt) -> Self {
        use gents_protocol::client_access::ClientAccessState;
        Self {
            request_id: receipt.request_id,
            node_did: receipt.node_did,
            client_did: receipt.client_did,
            state: match receipt.state {
                ClientAccessState::Pending => "pending",
                ClientAccessState::Approved => "approved",
                ClientAccessState::Rejected => "rejected",
                ClientAccessState::Revoked => "revoked",
            }
            .to_owned(),
        }
    }
}
