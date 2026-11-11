use anyhow::{anyhow, Context, Result};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use serde::Deserialize;

use crate::call::lifecycle::ToolCallLifecycle;
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_session::request::lifecycle::queue::{QueuePolicy, QueueSource, RequestQueue};
use gents_store::graphql::escape_graphql_string;

mod datetime_fields;
mod notification_delivery;
mod session_message;
#[cfg(test)]
pub use session_message::turn_assignment_handoff;
mod side_effects;

pub use notification_delivery::append_background_tool_completion;
pub(crate) use notification_delivery::append_background_tool_completion_for_parent;
pub(crate) use session_message::settle_session_message_row;
pub use session_message::{
    settle_rows_after_request_update, settle_running_session_message_rows_with_access,
};

use datetime_fields::agent_tool_call_datetime_update_fragment;
use side_effects::existing_tool_completion_notification;
