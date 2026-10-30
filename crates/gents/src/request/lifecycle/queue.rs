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
use crate::request::watcher::AgentRequest;

use super::materialize::EnqueuedAgentRequest;
use super::ExecutionOrigin;

mod atomic_inputs;
mod coalescing;
mod draining;
mod edit_submission;
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
pub(crate) use draining::drain_all_pending_session_requests_in_txn;
pub(crate) use draining::drain_automated_wakeups_in_txn;
pub use edit_submission::{submit_pending_user_edit, submit_pending_user_steer};
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
#[cfg(test)]
use input::queue_is_automated_wakeup;
pub(crate) use input::{
    background_wake_queue, is_automated_wakeup, row_is_automated_wakeup, row_queue,
};
pub(crate) use intake::{
    pending_ids_in_txn, pending_with_new_candidate_in_txn, steer_reachable, steering_inputs,
    steering_snapshot, SteerAdmissionRefusal,
};
#[cfg(test)]
pub(crate) use intake::{pending_in_txn, pending_rows_in_txn, steering_compatible};
pub(crate) use management::{
    apply_pending_user_edit_in_txn, persisted_replacements_in_txn,
    validate_pending_user_edit_in_txn, PersistedReplacements,
};
pub use management::{
    pending_steer_edit, pending_user_queue, pending_user_queue_projection,
    prepare_pending_user_edit, PendingQueueEntry, PendingQueueProjection, PendingQueueSnapshot,
    REMOVED_REASON, REPLACED_REASON,
};
#[cfg(test)]
use management::{replace_pending_user_messages, PendingEditActor};
pub use management::{PendingMessageEdit, PendingQueueEdit, PendingQueueReceipt};
#[cfg(test)]
use mutation::session_request_create_mutation;
use mutation::session_request_create_mutation_at_hop;
use mutation::SessionRequestFields;
pub use position::validate_position;
pub(crate) use position::{effective_slots, InvalidQueuePosition};
pub use submission::prepare_user_message_input;

#[derive(Debug)]
pub(crate) struct EnqueuedBackgroundCompletionInput {
    pub(crate) request: Option<EnqueuedAgentRequest>,
    pub(crate) message_sequence: u32,
    pub(crate) created_request: bool,
}

#[cfg(test)]
mod tests;
