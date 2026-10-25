use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceToolView {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ToolServiceTestResult {
    pub service_id: String,
    pub endpoint: String,
    pub status: String,
    pub tool_count: usize,
    pub tools: Vec<ToolServiceToolView>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TaskRunResult {
    pub request_doc_id: String,
    pub request_id: String,
    pub session_id: String,
    pub node_did: String,
    pub agent_id: String,
    pub lifecycle_state: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ChatSendResult {
    pub session_id: String,
    pub request_id: String,
    pub node_did: String,
    pub agent_id: Option<String>,
    /// Absent when the owner accepted the request.
    #[ts(optional = nullable)]
    pub outcome: Option<ChatSendOutcome>,
    #[ts(optional = nullable)]
    pub submission_receipt: Option<ChatSubmissionReceipt>,
    /// Recovery information does not change the retained request's delivery outcome.
    #[ts(optional = nullable)]
    pub delivery_error: Option<crate::error::BridgeError>,
}

/// Native receipt fields are echoed for acknowledgement; the native owner
/// verifies their exact binding before retiring any signed replay bytes.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatSubmissionReceipt {
    pub node_did: String,
    pub request_id: String,
    pub session_id: String,
    pub agent_id: Option<String>,
    pub envelope_hash: String,
    pub signed_payload_hash: String,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChatSubmissionRecovery {
    /// A missing local preparation is not proof of non-delivery. No resolved
    /// session is invented when the original call may still be preparing it.
    Unconfirmed {
        node_did: String,
        request_id: String,
    },
    Prepared {
        result: Box<ChatSendResult>,
    },
}

/// A send the owner has not confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ChatSendOutcome {
    /// The owner may hold the request; resend must reconcile this identity.
    DeliveryUnknown,
    /// The owner definitely rejected the retained, byte-identical signed
    /// request, so it was not delivered.
    NotDelivered,
}

impl From<gents_desktop_core::client::SubmittedRequest> for ChatSendResult {
    fn from(receipt: gents_desktop_core::client::SubmittedRequest) -> Self {
        Self {
            session_id: receipt.session_id,
            request_id: receipt.request_id,
            node_did: receipt.node_did,
            agent_id: receipt.agent_id,
            outcome: None,
            submission_receipt: None,
            delivery_error: None,
        }
    }
}

impl ChatSendResult {
    pub(crate) fn with_outcome(
        receipt: gents_desktop_core::client::SubmittedRequest,
        outcome: ChatSendOutcome,
    ) -> Self {
        Self {
            outcome: Some(outcome),
            ..receipt.into()
        }
    }
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ConfiguredActionKind {
    Task,
    Schedule,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ConfiguredActionView {
    pub kind: ConfiguredActionKind,
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestOutputView {
    pub node_did: String,
    pub session_id: String,
    pub request_id: String,
    pub output: RequestOutputStateView,
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RequestOutputStateView {
    Loading,
    TerminalMessage { content: String },
    TerminalNoMessage,
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientUpdateEvent {
    #[ts(type = "string")]
    pub reason: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub store_version: Option<u64>,
}

impl ClientUpdateEvent {
    pub fn coarse(reason: &'static str) -> Self {
        Self {
            reason,
            store_version: None,
        }
    }

    pub fn store(notice: gents_desktop_core::client::StoreUpdateNotice) -> Self {
        Self {
            reason: "store",
            store_version: Some(notice.revision.store_version),
        }
    }
}
