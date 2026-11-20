use std::collections::BTreeSet;

use gents_protocol::request_lifecycle::RequestLifecycleState;

use super::rows::{RetrySummary, TimelineInferenceCallRow, TimelineRequestRow};
use super::values::timestamp_millis;

pub(super) fn sorted_inference_calls(
    inference_calls: Vec<TimelineInferenceCallRow>,
    included_request_ids: &BTreeSet<String>,
) -> Vec<TimelineInferenceCallRow> {
    let mut inference_calls = inference_calls
        .into_iter()
        .filter(|call| included_request_ids.contains(&call.request_id))
        .collect::<Vec<_>>();
    inference_calls.sort_by_key(|call| {
        (
            call.request_id.clone(),
            call.call_seq,
            call.queued_at
                .as_deref()
                .and_then(timestamp_millis)
                .unwrap_or(i64::MIN),
            call.call_id.clone(),
        )
    });
    inference_calls
}

pub(super) fn apply_retry_summaries(
    root_request: &mut TimelineRequestRow,
    requests: &mut [TimelineRequestRow],
    inference_calls: &[TimelineInferenceCallRow],
) {
    root_request.retry_summary = retry_summary_for_request(root_request, inference_calls);
    for request in requests {
        request.retry_summary = retry_summary_for_request(request, inference_calls);
    }
}

/// `RunTimelineRetry.summary`: a failed attempt is a retry when the request
/// made a later attempt by call sequence, which resumed executions continue.
pub(super) fn retry_summary_for_request(
    request: &TimelineRequestRow,
    inference_calls: &[TimelineInferenceCallRow],
) -> RetrySummary {
    let request_calls = inference_calls
        .iter()
        .filter(|call| call.request_id == request.request_id)
        .filter(|call| call.call_kind == "inference")
        .collect::<Vec<_>>();
    let retried = request_calls
        .iter()
        .copied()
        .filter(|call| {
            call.call_state == "failed"
                && request_calls
                    .iter()
                    .any(|later| call.call_seq < later.call_seq)
        })
        .collect::<Vec<_>>();
    let latest = retried.iter().copied().reduce(|kept, call| {
        if kept.call_seq < call.call_seq {
            call
        } else {
            kept
        }
    });
    let retry_count = u32::try_from(retried.len()).unwrap_or(u32::MAX);
    RetrySummary {
        retry_count,
        last_transient_error: latest.and_then(|call| call.failure_reason.clone()),
        recovered: request_completed(request) && retry_count > 0,
    }
}

fn request_completed(request: &TimelineRequestRow) -> bool {
    request.lifecycle_state == Some(RequestLifecycleState::Completed)
}
