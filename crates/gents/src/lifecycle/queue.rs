#![allow(dead_code)]

use anyhow::Context;
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
pub(crate) use intake::{pending_ids_in_txn, steering_inputs, steering_snapshot};
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

type Result<T, E = QueueError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("goal continuation sequence must be positive")]
    NonPositiveGoalContinuationSequence,
    #[error(transparent)]
    Config(#[from] crate::config_client::ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] crate::graphql::GraphqlResponseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Durable(#[from] crate::trigger_engine::durable::DurableTriggerError),
    #[error(transparent)]
    Input(#[from] gents_protocol::request_input::RequestInputError),
    #[error(transparent)]
    Watcher(#[from] crate::watcher::WatcherError),
    #[error(transparent)]
    PendingInputChanged(#[from] PendingInputChanged),
    #[error(transparent)]
    InvalidQueuePosition(#[from] InvalidQueuePosition),
    #[error("pending AgentRequest row is missing _docID")]
    PendingRequestMissingDocId,
    #[error("pending input lacks physical identity")]
    PendingInputMissingDocId,
    #[error("claim time is not RFC 3339")]
    ClaimTimeNotRfc3339(#[source] chrono::ParseError),
    #[error("pending input arrival order is incomplete")]
    PendingInputArrivalIncomplete,
    #[error("folded input publication omitted affected rows")]
    FoldedPublicationOmittedRows,
    #[error("folded input publication changed multiple rows")]
    FoldedPublicationChangedRows,
    #[error("folded request receipt omitted request_id")]
    FoldedReceiptMissingRequestId,
    #[error("replayed folded publication was not consumed by its request")]
    FoldedReplayNotConsumed,
    #[error("decode folded requests")]
    DecodeFoldedRequests(#[source] serde_json::Error),
    #[error("folded request {request_id} crossed its claimed request's requester")]
    FoldedRequesterMismatch { request_id: String },
    #[error("folded AgentRequest row is missing _docID")]
    FoldedRowMissingDocId,
    #[error("folded requests of {request_id} lack a queue arrival")]
    FoldedRequestsWithoutArrival { request_id: String },
    #[error("folded arrival is ambiguous")]
    FoldedArrivalAmbiguous,
    #[error("decode pending session requests")]
    DecodePendingRequests(#[source] serde_json::Error),
    #[error("pending message has no physical identity")]
    PendingMessageMissingDocId,
    #[error("pending queue arrival is incomplete")]
    PendingQueueArrivalIncomplete,
    #[error("pending queue arrival changed")]
    PendingQueueArrivalChanged,
    #[error("pending AgentRequest query omitted rows")]
    PendingQueryOmittedRows,
    #[error("pending AgentRequest drain mutation omitted affected rows")]
    DrainMutationOmittedRows,
    #[error("pending AgentRequest drain receipt omitted _docID")]
    DrainReceiptMissingDocId,
    #[error("pending AgentRequest drain mutation returned unexpected rows")]
    DrainMutationUnexpectedRows,
    #[error("pending AgentRequest drain mutation updated another physical request")]
    DrainMutationUpdatedAnother,
    #[error("queue row missing physical identity")]
    QueueRowMissingIdentity,
    #[error("node_did is required")]
    NodeDidRequired,
    #[error("session_id is required")]
    SessionIdRequired,
    #[error("user messages require an ordinary user queue entry")]
    UserEntryNotOrdinary,
    #[error("session has conflicting active requests")]
    ConflictingActiveRequests,
    #[error("cannot enqueue same-session request: parent {request_id} has no agent_id")]
    SameSessionParentWithoutAgent { request_id: String },
    #[error("pending row lacks physical identity")]
    PendingRowMissingIdentity,
    #[error("The message queue changed. Refresh it and try again.")]
    MessageQueueChanged,
    #[error("Select a pending message first.")]
    NoPendingMessageSelected,
    #[error("Selected messages are no longer adjacent in the queue.")]
    SelectedMessagesNotAdjacent,
    #[error("A selected message has started or cannot be edited by this user.")]
    SelectedMessageNotEditable,
    #[error("Messages with different execution settings cannot be reordered together.")]
    MixedExecutionSettings,
    #[error("Replacement must preserve the selected message count.")]
    ReplacementCountMismatch,
    #[error("Replacement messages must name each selected message once.")]
    ReplacementMessageNotSelected,
    #[error("A message cannot be empty. Remove it instead.")]
    EmptyReplacementMessage,
    #[error("selected message has no physical identity")]
    SelectedMessageMissingIdentity,
    #[error("selected slot has no identity")]
    SelectedSlotMissingIdentity,
    #[error("selected slot has no queue input")]
    SelectedSlotMissingQueue,
    #[error("selected message has no queue input")]
    SelectedMessageMissingQueue,
    #[error("Steering input cannot be changed back to queued delivery.")]
    SteeringRevertedToQueue,
    #[error("message has no agent")]
    MessageMissingAgent,
    #[error("replacement identity lookup missing")]
    ReplacementIdentityLookupMissing,
    #[error("replacement lacks physical identity")]
    ReplacementMissingIdentity,
    #[error("A selected message started before this edit. Refresh the queue.")]
    SelectedMessageStarted,
    #[error("This message is no longer pending.")]
    MessageNoLongerPending,
    #[error("This message cannot be steered or is already steering.")]
    MessageNotSteerable,
    #[error("This message cannot pass earlier input with different execution settings.")]
    SteerMixedExecutionSettings,
    #[error("{reason}")]
    RequestCreate { reason: &'static str },
    #[error("Runtime rejected pending edit: {reason}")]
    RuntimeRejectedEdit { reason: String },
}

impl crate::config_client::TransactionFailure for QueueError {
    fn config_client_error(&self) -> Option<&crate::config_client::ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            Self::Graphql(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Durable(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Watcher(error) => {
                crate::config_client::TransactionFailure::config_client_error(error)
            }
            _ => None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct EnqueuedBackgroundCompletionInput {
    pub(crate) request: Option<EnqueuedAgentRequest>,
    pub(crate) message_sequence: u32,
    pub(crate) created_request: bool,
}

#[cfg(test)]
mod tests;
