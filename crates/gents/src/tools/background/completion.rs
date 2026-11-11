use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::background_completion::is_background_completion_notification_message_key;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::request::lifecycle::queue::{QueuePolicy, QueueSource, RequestQueue};
use crate::store::graphql::escape_graphql_string;
use crate::tools::call::lifecycle::{FailureClass, ToolCallLifecycle};

const AGENT_REQUEST_COLLECTION: &str = "AgentRequest";

pub(crate) fn notification_receipt_keys(encoded: &str) -> Result<Vec<String>> {
    let keys: Vec<String> =
        serde_json::from_str(encoded).context("invalid wake notification receipt")?;
    anyhow::ensure!(
        keys.iter()
            .all(|key| is_background_completion_notification_message_key(key)),
        "wake receipt names a non-notification message"
    );
    anyhow::ensure!(
        keys.iter().collect::<std::collections::BTreeSet<_>>().len() == keys.len(),
        "wake receipt contains duplicate notification keys"
    );
    Ok(keys)
}

mod datetime_fields;
mod notification_delivery;
mod observer;
mod session_message;
#[cfg(test)]
pub(crate) use session_message::turn_assignment_handoff;
mod side_effects;

pub(crate) use notification_delivery::{
    append_background_tool_completion, append_background_tool_completion_for_parent,
};
pub(crate) use observer::run_background_completion_observer;
pub(crate) use session_message::{
    settle_running_session_message_rows_with_access, settle_session_message_row,
};

use datetime_fields::agent_tool_call_datetime_update_fragment;
use side_effects::existing_tool_completion_notification;
