//! Canonical typed queue input. The JSON metadata wrapper and its hints
//! parser are deleted; queue facts are the typed `RequestInput.queue`
//! decoded by `gents_protocol::row::AgentRequestRow`.

use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestInput, RequestQueue};
use gents_protocol::row::AgentRequestRow;

pub(crate) const BACKGROUND_COMPLETION_WAKE_VERSION: u32 = 1;
const STEERING_INPUT_MESSAGE_PREFIX: &str = "steering-input:";

pub(crate) fn queue_is_automated_wakeup(queue: &RequestQueue) -> bool {
    matches!(queue.source, QueueSource::BackgroundCompletion)
        && queue.policy == QueuePolicy::Coalesce
        && queue
            .key
            .as_deref()
            .is_some_and(|key| !key.trim().is_empty())
}

pub(crate) fn is_automated_wakeup(input: &RequestInput) -> bool {
    input.queue.as_ref().is_some_and(queue_is_automated_wakeup)
}

/// The coalescing domain of a row's queue input, when it is a keyed
/// background-completion wake.
pub(crate) fn row_queue(row: &AgentRequestRow) -> Option<&RequestQueue> {
    row.input.as_ref()?.queue.as_ref()
}

pub(crate) fn row_is_automated_wakeup(row: &AgentRequestRow) -> bool {
    row_queue(row).is_some_and(queue_is_automated_wakeup)
}

pub(crate) fn row_is_subagent_owned_queue(row: &AgentRequestRow) -> bool {
    row_queue(row).is_some_and(|queue| {
        matches!(queue.source, QueueSource::Steering) || queue_is_automated_wakeup(queue)
    })
}

/// Stamp the durable wake format marker on a freshly created background wake.
/// An incoming caller queue never authorizes the internal wake marker.
pub(crate) fn background_wake_queue(
    queue: &RequestQueue,
    queued_after_request_id: Option<String>,
) -> RequestQueue {
    RequestQueue {
        source: QueueSource::BackgroundCompletion,
        policy: QueuePolicy::Coalesce,
        key: queue.key.clone(),
        queued_after_request_id,
        interrupted_request_id: None,
        background_completion_wake_version: Some(BACKGROUND_COMPLETION_WAKE_VERSION),
    }
}

pub fn steering_input_message_key(request_id: &str) -> String {
    format!("{STEERING_INPUT_MESSAGE_PREFIX}{request_id}")
}

pub fn is_steering_input_message_key(message_key: &str) -> bool {
    message_key.starts_with(STEERING_INPUT_MESSAGE_PREFIX)
}
