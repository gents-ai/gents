use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::BridgeError;
use crate::types::{
    ChatSendRequest, DesktopInterruptRequest, EnrollmentStatusRequest, PendingMessageSteerRequest,
    PendingQueueEditRequest, SessionRenameRequest,
};

/// Change the version for incompatible framing or operation semantics.
/// Additional commands are negotiated through the initialization inventory.
/// Typed captures require a host that preserves their submitted snapshots.
pub const PROTOCOL_VERSION: u32 = 4;
/// The private store-key channel completes before ordinary host framing begins.
/// Its failures use the canonical bridge error contract and never raw causes.
pub const EDITOR_STORE_BOOTSTRAP_VERSION: u32 = 2;
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PENDING_CALLS: usize = 32;

#[derive(Debug, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum ClientMessage {
    Hello { version: u32 },
    Call { id: u32, command: Value },
    Cancel { id: u32 },
}

#[derive(Debug, Deserialize, TS)]
#[serde(tag = "name", content = "args")]
#[serde(deny_unknown_fields)]
pub enum ClientCommand {
    #[serde(rename = "client_backend_authentication")]
    BackendAuthentication {
        request: crate::accounts::BackendAuthenticationRequest,
    },
    #[serde(rename = "client_backend_authentication_save")]
    BackendAuthenticationSave {
        request: crate::accounts::BackendAuthenticationSaveRequest,
    },
    #[serde(rename = "client_runtime_is_local")]
    RuntimeIsLocal {
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "client_inference_setup_catalog")]
    InferenceCatalog {},
    #[serde(rename = "client_inference_setup_state")]
    InferenceSetupState {
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "client_inference_models_discover")]
    InferenceModelsDiscover {
        request: crate::inference_setup::InferenceDiscoveryRequest,
    },
    #[serde(rename = "client_inference_model_recommendation")]
    InferenceModelRecommendation {
        request: crate::inference_setup::InferenceRecommendationRequest,
    },
    #[serde(rename = "client_agent_model_select")]
    AgentModelSelect {
        request: crate::types::AgentModelSelectionRequest,
    },
    #[serde(rename = "client_config_apply")]
    ConfigApply {
        request: Box<crate::types::ConfigComponentsApplyRequest>,
    },
    #[serde(rename = "client_provider_accounts_list")]
    AccountsList {
        request: crate::accounts::ProviderAccountsRequest,
    },
    #[serde(rename = "client_provider_account_sign_in")]
    AccountSignIn {
        request: crate::accounts::ProviderSignInRequest,
    },
    #[serde(rename = "client_provider_account_disconnect")]
    AccountDisconnect {
        request: crate::accounts::ProviderAccountDisconnectRequest,
    },
    #[serde(rename = "client_provider_account_rename")]
    AccountRename {
        request: crate::accounts::ProviderAccountRenameRequest,
    },
    #[serde(rename = "client_provider_account_remove")]
    AccountRemove {
        request: crate::accounts::ProviderAccountRemoveRequest,
    },
    #[serde(rename = "client_provider_account_retry_save")]
    AccountRetrySave {
        request: crate::accounts::ProviderAccountRetrySaveRequest,
    },
    #[serde(rename = "client_config_catalog")]
    ConfigCatalog {
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "client_config_read")]
    ConfigRead {
        #[serde(rename = "nodeDid")]
        node_did: String,
        collection: String,
        id: String,
    },
    #[serde(rename = "client_config_save")]
    ConfigSave {
        #[serde(rename = "nodeDid")]
        node_did: String,
        collection: String,
        id: String,
        revision: gents_desktop_core::client::ConfigEditorRevision,
        content: String,
    },
    #[serde(rename = "client_action_catalog")]
    ActionCatalog {
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "client_start_task")]
    StartTask {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(rename = "argumentsJson")]
        /// Parsed natively to avoid a JavaScript number round trip.
        arguments_json: String,
    },
    #[serde(rename = "client_run_schedule")]
    RunSchedule {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "scheduleId")]
        schedule_id: String,
    },
    #[serde(rename = "desktop_session_file_inventory")]
    SessionFileInventory {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(default)]
        #[ts(optional)]
        action: Option<gents_protocol::file_inventory::FileInventoryReadAction>,
    },
    #[serde(rename = "desktop_inventory_file_change")]
    InventoryFileChange {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        reference: gents_protocol::file_inventory::FileInventoryImageReference,
        #[serde(rename = "retryId", default)]
        #[ts(optional)]
        retry_id: Option<String>,
    },
    #[serde(rename = "desktop_tool_file_changes")]
    ToolFileChanges {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "toolCallDocId")]
        tool_call_doc_id: String,
        #[serde(rename = "retryId", default)]
        #[ts(optional)]
        retry_id: Option<String>,
    },
    #[serde(rename = "desktop_request_output")]
    RequestOutput {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "sessionId")]
        session_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
    },
    #[serde(rename = "client_speech_services")]
    SpeechServices {
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "client_speech_tools")]
    SpeechTools {
        #[serde(rename = "nodeDid")]
        node_did: String,
        #[serde(rename = "serviceId")]
        service_id: String,
    },
    #[serde(rename = "client_speech_recognize")]
    SpeechRecognize {
        request: super::speech::SpeechRecognizeRequest,
    },
    #[serde(rename = "client_speech_synthesize")]
    SpeechSynthesize {
        request: super::speech::SpeechSynthesizeRequest,
    },
    #[serde(rename = "client_connect_local")]
    ConnectLocal {
        #[serde(rename = "nodeHome")]
        node_home: Option<String>,
    },
    #[serde(rename = "client_workspace_permissions")]
    WorkspacePermissions {
        request: super::permissions::WorkspacePermissionsRequest,
    },
    #[serde(rename = "client_workspace_permissions_save")]
    WorkspacePermissionsSave {
        request: super::permissions::WorkspacePermissionsSaveRequest,
    },
    #[serde(rename = "client_plugin_approval_subscribe")]
    ApprovalSubscribe {
        #[serde(rename = "nodeDid")]
        node_did: String,
        token: String,
    },
    #[serde(rename = "client_plugin_approval_unsubscribe")]
    ApprovalUnsubscribe { token: String },
    #[serde(rename = "client_plugin_approval_pending")]
    ApprovalPending {
        #[serde(rename = "nodeDid")]
        node_did: String,
        token: String,
    },
    #[serde(rename = "client_plugin_approval_decide")]
    ApprovalDecide {
        #[serde(rename = "nodeDid")]
        node_did: String,
        token: String,
        id: String,
        decision: String,
    },
    #[serde(rename = "desktop_client_snapshot")]
    Snapshot {},
    #[serde(rename = "desktop_peer_enroll_status")]
    Enroll { request: EnrollmentStatusRequest },
    #[serde(rename = "desktop_session_snapshot")]
    SessionSnapshot(SessionSnapshotArgs),
    #[serde(rename = "desktop_session_live_delta")]
    SessionLiveDelta(SessionLiveDeltaArgs),
    #[serde(rename = "desktop_session_rows")]
    SessionRows(SessionRowsArgs),
    #[serde(rename = "desktop_session_goal")]
    SessionGoal(SessionGoalArgs),
    #[serde(rename = "desktop_session_provenance")]
    SessionProvenance {
        request: crate::types::DesktopSessionProvenanceRequest,
    },
    #[serde(rename = "desktop_goal_control")]
    GoalControl {
        request: Box<crate::types::GoalControlRequest>,
    },
    #[serde(rename = "desktop_chat_send")]
    Send { request: ChatSendRequest },
    #[serde(rename = "desktop_chat_submission_recover")]
    RecoverSubmission { request: ChatSendRequest },
    #[serde(rename = "desktop_chat_submission_acknowledge")]
    AcknowledgeSubmission {
        receipt: crate::types::ChatSubmissionReceipt,
    },
    #[serde(rename = "desktop_request_retry")]
    Retry {
        #[serde(rename = "requestId")]
        request_id: String,
        #[serde(rename = "nodeDid")]
        node_did: String,
    },
    #[serde(rename = "desktop_pending_queue_edit")]
    EditPending { request: PendingQueueEditRequest },
    #[serde(rename = "desktop_pending_message_steer")]
    SteerPending { request: PendingMessageSteerRequest },
    #[serde(rename = "desktop_session_fork_before_message")]
    ForkBeforeMessage {
        request: crate::types::SessionForkBeforeRequest,
    },
    #[serde(rename = "desktop_session_rename")]
    Rename { request: SessionRenameRequest },
    #[serde(rename = "desktop_interrupt_request")]
    Interrupt { request: DesktopInterruptRequest },
}

impl ClientCommand {
    pub const NAMES: &'static [&'static str] = &[
        "client_backend_authentication",
        "client_backend_authentication_save",
        "client_runtime_is_local",
        "client_inference_setup_catalog",
        "client_inference_setup_state",
        "client_agent_model_select",
        "client_inference_models_discover",
        "client_inference_model_recommendation",
        "client_config_apply",
        "client_provider_accounts_list",
        "client_provider_account_sign_in",
        "client_provider_account_disconnect",
        "client_provider_account_rename",
        "client_provider_account_remove",
        "client_provider_account_retry_save",
        "client_config_catalog",
        "client_config_read",
        "client_config_save",
        "client_action_catalog",
        "client_start_task",
        "client_run_schedule",
        "desktop_request_output",
        "desktop_tool_file_changes",
        "desktop_inventory_file_change",
        "desktop_session_file_inventory",
        "client_speech_services",
        "client_speech_tools",
        "client_speech_recognize",
        "client_speech_synthesize",
        "client_connect_local",
        "client_workspace_permissions",
        "client_workspace_permissions_save",
        "client_plugin_approval_subscribe",
        "client_plugin_approval_unsubscribe",
        "client_plugin_approval_pending",
        "client_plugin_approval_decide",
        "desktop_client_snapshot",
        "desktop_peer_enroll_status",
        "desktop_session_snapshot",
        "desktop_session_live_delta",
        "desktop_session_rows",
        "desktop_session_goal",
        "desktop_session_provenance",
        "desktop_goal_control",
        "desktop_chat_send",
        "desktop_chat_submission_recover",
        "desktop_chat_submission_acknowledge",
        "desktop_request_retry",
        "desktop_pending_queue_edit",
        "desktop_pending_message_steer",
        "desktop_session_fork_before_message",
        "desktop_session_rename",
        "desktop_interrupt_request",
    ];

    pub fn is_cancellable(&self) -> bool {
        matches!(
            self,
            Self::Snapshot {}
                | Self::BackendAuthentication { .. }
                | Self::InferenceCatalog {}
                | Self::InferenceSetupState { .. }
                | Self::RuntimeIsLocal { .. }
                | Self::InferenceModelsDiscover { .. }
                | Self::InferenceModelRecommendation { .. }
                | Self::AccountsList { .. }
                | Self::ConfigCatalog { .. }
                | Self::ConfigRead { .. }
                | Self::ActionCatalog { .. }
                | Self::RequestOutput { .. }
                | Self::ToolFileChanges { .. }
                | Self::SessionFileInventory { .. }
                | Self::InventoryFileChange { .. }
                | Self::SpeechServices { .. }
                | Self::SpeechTools { .. }
                | Self::SpeechRecognize { .. }
                | Self::SpeechSynthesize { .. }
                | Self::WorkspacePermissions { .. }
                | Self::ApprovalPending { .. }
                | Self::SessionSnapshot(_)
                | Self::SessionLiveDelta(_)
                | Self::SessionGoal(_)
                | Self::SessionProvenance { .. }
        )
    }
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SessionSnapshotArgs {
    pub session_id: String,
    pub node_did: String,
    #[ts(optional = nullable)]
    pub request_id: Option<String>,
    #[ts(optional = nullable)]
    pub timeline_limit: Option<usize>,
    #[ts(optional = nullable)]
    pub timeline_before_item_key: Option<String>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionRowsArgs {
    pub session_id: String,
    pub node_did: String,
    pub window: crate::types::SessionRowsWindow,
    #[ts(optional = nullable)]
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionGoalArgs {
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct SessionLiveDeltaArgs {
    pub session_id: String,
    pub node_did: String,
    pub request_id: String,
    pub base_live_cursor: String,
    pub base_content_byte_len: usize,
    pub base_content_hash: String,
    pub base_reasoning_byte_len: usize,
    pub base_reasoning_hash: String,
}

#[derive(Debug, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HostMessage {
    Hello {
        version: u32,
        commands: Vec<String>,
        #[serde(rename = "maxFrameBytes")]
        max_frame_bytes: usize,
    },
    Response {
        id: u32,
        result: Option<Value>,
        error: Option<BridgeError>,
    },
    Updated {
        #[ts(type = "number")]
        sequence: u64,
        payload: crate::types::ClientUpdateEvent,
    },
}

impl HostMessage {
    pub fn response(id: u32, result: Result<Value, BridgeError>) -> Self {
        match result {
            Ok(value) => Self::Response {
                id,
                result: Some(value),
                error: None,
            },
            Err(error) => Self::Response {
                id,
                result: None,
                error: Some(error),
            },
        }
    }
}
