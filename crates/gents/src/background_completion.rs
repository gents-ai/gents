use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::{QueuePolicy, QueueSource, RequestQueue};
use crate::tool_call_lifecycle::{FailureClass, ToolCallLifecycle};

const AGENT_REQUEST_COLLECTION: &str = "AgentRequest";
pub const BACKGROUND_COMPLETION_WAKE_PROMPT: &str =
    "Review the new background completion results and continue the task if needed.";
const BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX: &str =
    "background-completion-notification:";

pub(crate) fn background_completion_notification_message_key(
    stable_id: &str,
    kind: &str,
) -> String {
    format!("{BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX}{stable_id}:{kind}")
}

pub fn is_background_completion_notification_message_key(message_key: &str) -> bool {
    message_key.starts_with(BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX)
}

#[derive(Debug, thiserror::Error)]
pub enum BackgroundCompletionError {
    #[error("invalid wake notification receipt")]
    InvalidReceipt(#[source] serde_json::Error),
    #[error("wake receipt names a non-notification message")]
    NonNotificationReceipt,
    #[error("wake receipt contains duplicate notification keys")]
    DuplicateReceiptKey,
}

pub(crate) fn notification_receipt_keys(
    encoded: &str,
) -> std::result::Result<Vec<String>, BackgroundCompletionError> {
    let keys: Vec<String> =
        serde_json::from_str(encoded).map_err(BackgroundCompletionError::InvalidReceipt)?;
    if !keys
        .iter()
        .all(|key| is_background_completion_notification_message_key(key))
    {
        return Err(BackgroundCompletionError::NonNotificationReceipt);
    }
    if keys.iter().collect::<std::collections::BTreeSet<_>>().len() != keys.len() {
        return Err(BackgroundCompletionError::DuplicateReceiptKey);
    }
    Ok(keys)
}

mod datetime_fields;
mod notification_delivery;
mod observer;
mod rendering;
mod session_message;
#[cfg(test)]
pub(crate) use session_message::turn_assignment_handoff;
mod side_effects;

pub(crate) use notification_delivery::append_background_tool_completion;
pub(crate) use observer::run_background_completion_observer;
pub(crate) use session_message::{settle_running_session_message_rows, settle_session_message_row};

use datetime_fields::agent_tool_call_datetime_update_fragment;
use rendering::tool_completion_presentation;
use side_effects::existing_tool_completion_notification;
