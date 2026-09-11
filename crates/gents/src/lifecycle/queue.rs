#![allow(dead_code)]

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::row::AgentRequestRow;
#[cfg(test)]
use serde::Deserialize;
use serde_json::Value;

use crate::config_client::ConfigApplyTxn;
use crate::graphql::{escape_graphql_string, response_has_documents};
use crate::session;
use crate::watcher::AgentRequest;

use super::materialize::EnqueuedAgentRequest;
use super::ExecutionOrigin;

mod atomic_inputs;
mod coalescing;
mod draining;
mod enqueue;
mod goal_continuation;
mod input;
mod mutation;

pub(crate) use atomic_inputs::persist_background_completion_with_message;
use atomic_inputs::steering_transaction_attempt;
#[cfg(test)]
use atomic_inputs::transaction_created_doc_id;
pub use coalescing::reconcile_coalesced_pending_request;
use coalescing::{
    parent_behavior_id, queue_row_to_enqueued_request, row_matches_coalesced_source_and_key,
};
pub use draining::drain_automated_wakeups;
pub(crate) use draining::drain_subagent_owned_queue;
pub use enqueue::enqueue_local_steering_request;
pub(crate) use enqueue::enqueue_steering_request_with_message;
pub use gents_protocol::request_input::{
    GoalContinuationInput, QueuePolicy, QueueSource, RequestInput, RequestQueue,
};
pub(crate) use goal_continuation::{
    goal_continuation_behavior, goal_continuation_identity, prepare_goal_continuation,
};
pub(crate) use input::{
    background_wake_queue, is_automated_wakeup, row_is_automated_wakeup,
    row_is_subagent_owned_queue, row_queue,
};
pub use input::{is_steering_input_message_key, steering_input_message_key};
#[cfg(test)]
use input::{queue_is_automated_wakeup, BACKGROUND_COMPLETION_WAKE_VERSION};
use mutation::session_request_create_mutation;

#[derive(Debug)]
pub(crate) struct EnqueuedBackgroundCompletionInput {
    /// None is a durable notification without a background wake request.
    pub(crate) request: Option<EnqueuedAgentRequest>,
    pub(crate) message_sequence: u32,
    pub(crate) created_request: bool,
}

#[cfg(test)]
mod tests;
