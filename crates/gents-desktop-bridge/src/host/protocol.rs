use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::error::BridgeError;
use crate::types::{
    ChatSendRequest, DesktopInterruptRequest, EnrollmentStatusRequest, PendingQueueEditRequest,
    SessionRenameRequest,
};

/// Change the version for incompatible framing or operation semantics.
/// Additional commands are negotiated through the initialization inventory.
pub const PROTOCOL_VERSION: u32 = 2;
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
    #[serde(rename = "client_connect_local")]
    ConnectLocal {
        #[serde(rename = "nodeHome")]
        node_home: Option<String>,
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
    #[serde(rename = "desktop_session_goal")]
    SessionGoal(SessionGoalArgs),
    #[serde(rename = "desktop_goal_control")]
    GoalControl {
        request: crate::types::GoalControlRequest,
    },
    #[serde(rename = "desktop_chat_send")]
    Send { request: ChatSendRequest },
    #[serde(rename = "desktop_pending_queue_edit")]
    EditPending { request: PendingQueueEditRequest },
    #[serde(rename = "desktop_session_rename")]
    Rename { request: SessionRenameRequest },
    #[serde(rename = "desktop_interrupt_request")]
    Interrupt { request: DesktopInterruptRequest },
}

impl ClientCommand {
    pub const NAMES: &'static [&'static str] = &[
        "client_connect_local",
        "client_plugin_approval_subscribe",
        "client_plugin_approval_unsubscribe",
        "client_plugin_approval_pending",
        "client_plugin_approval_decide",
        "desktop_client_snapshot",
        "desktop_peer_enroll_status",
        "desktop_session_snapshot",
        "desktop_session_live_delta",
        "desktop_session_goal",
        "desktop_goal_control",
        "desktop_chat_send",
        "desktop_pending_queue_edit",
        "desktop_session_rename",
        "desktop_interrupt_request",
    ];

    pub fn is_read(&self) -> bool {
        matches!(
            self,
            Self::Snapshot {}
                | Self::ApprovalPending { .. }
                | Self::SessionSnapshot(_)
                | Self::SessionLiveDelta(_)
                | Self::SessionGoal(_)
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
