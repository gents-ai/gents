#![allow(dead_code)]

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
#[cfg(test)]
use serde::Deserialize;
use serde_json::Value;

use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::watcher::AgentRequest;

use super::materialize::EnqueuedAgentRequest;
use super::ExecutionOrigin;

mod atomic_inputs;
mod coalescing;
mod draining;
mod enqueue;
mod folding;
mod goal_continuation;
mod input;
mod intake;
mod management;
mod mutation;
mod position;
mod submission;

pub(crate) use atomic_inputs::next_append_sequence_in_transaction;
#[cfg(test)]
pub(crate) use atomic_inputs::persist_background_completion_with_message;
pub(crate) use atomic_inputs::persist_background_completion_with_message_canonical;
#[cfg(test)]
pub(crate) use atomic_inputs::persist_background_completion_with_message_waking;
use atomic_inputs::steering_transaction_attempt;
#[cfg(test)]
use atomic_inputs::transaction_created_doc_id;
pub(crate) use atomic_inputs::ToolNotificationPublication;
pub use coalescing::reconcile_coalesced_pending_request;
pub(crate) use coalescing::supersede_pending_mutation;
use coalescing::{
    parent_agent_id, queue_row_to_enqueued_request, row_matches_coalesced_source_and_key,
};
pub(crate) use draining::drain_automated_wakeups_in_txn;
pub use enqueue::enqueue_local_steering_request;
#[cfg(test)]
pub(crate) use enqueue::enqueue_steering_request;
pub(crate) use folding::PendingInputChanged;
pub use folding::FOLDED_REASON;
pub(crate) use folding::{
    consume_folded_in_txn, ensure_folded_consumed_in_txn, fold_candidates, folded_input_key,
    heads_user_turn, load_consumed_folded_inputs, select_fold_in_claim_txn, FoldedConsumption,
    FoldedInput,
};
pub use gents_protocol::request_input::{
    GoalContinuationInput, QueuePolicy, QueueSource, RequestInput, RequestQueue,
};
pub(crate) use goal_continuation::{
    goal_continuation_agent, goal_continuation_identity, prepare_goal_continuation,
};
pub(crate) use input::{
    background_wake_queue, is_automated_wakeup, row_is_automated_wakeup, row_queue,
};
#[cfg(test)]
use input::{queue_is_automated_wakeup, BACKGROUND_COMPLETION_WAKE_VERSION};
pub(crate) use intake::{pending_ids_in_txn, steering_inputs, steering_snapshot};
pub(crate) use management::{apply_pending_user_edit_in_txn, validate_pending_user_edit_in_txn};
pub use management::{
    pending_user_queue, prepare_pending_user_edit, PendingQueueEntry, PendingQueueSnapshot,
};
pub use management::{
    replace_pending_user_messages, PendingMessageEdit, PendingQueueEdit, PendingQueueReceipt,
};
#[cfg(test)]
use mutation::session_request_create_mutation;
use mutation::session_request_create_mutation_at_hop;
pub use position::validate_position;
pub(crate) use position::{effective_slots, InvalidQueuePosition};
pub use submission::prepare_user_message_input;

#[derive(Debug)]
pub(crate) struct EnqueuedBackgroundCompletionInput {
    /// None is a durable notification without a background wake request.
    pub(crate) request: Option<EnqueuedAgentRequest>,
    pub(crate) message_sequence: u32,
    pub(crate) created_request: bool,
}

#[cfg(test)]
mod tests;
