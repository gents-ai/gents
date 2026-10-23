use rapidhash::RapidHashMap;
use std::time::{Duration, Instant};

use super::AgentRequest;

const MAX_PROCESSED_IDS: usize = 10_000;
pub(super) const GOSSIP_FALLBACK_POLL: Duration = Duration::from_secs(30);
pub(super) const PROCESSED_REQUEST_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProcessedMark {
    pub(super) at: Instant,
    pub(super) queue_session: Option<String>,
}

pub(super) type ProcessedRequests = RapidHashMap<String, ProcessedMark>;

pub(super) fn prune_processed_requests(
    processed_request_ids: &mut ProcessedRequests,
    now: Instant,
) {
    processed_request_ids
        .retain(|_, mark| now.saturating_duration_since(mark.at) < PROCESSED_REQUEST_COOLDOWN);

    if processed_request_ids.len() > MAX_PROCESSED_IDS {
        tracing::info!(
            count = processed_request_ids.len(),
            "pruning processed request ID set"
        );
        processed_request_ids.clear();
    }
}

pub(super) fn request_is_cooling_down(
    processed_request_ids: &mut ProcessedRequests,
    request_id: &str,
    now: Instant,
) -> bool {
    match processed_request_ids.get(request_id).map(|mark| mark.at) {
        Some(processed_at)
            if now.saturating_duration_since(processed_at) < PROCESSED_REQUEST_COOLDOWN =>
        {
            true
        }
        Some(_) => {
            processed_request_ids.remove(request_id);
            false
        }
        None => false,
    }
}

/// Lean `EventDelivery.Watcher.releasedBy`: delivering a session's queue head
/// releases the marks of that session's other requests. A request delivered
/// earlier and then overtaken by a same-second row that sorts ahead of it lost
/// its claim as `Queued` and stays pending; without the release it would wait
/// out the cooldown after its blocker terminalizes.
fn mark_processed(
    processed_request_ids: &mut ProcessedRequests,
    request: &AgentRequest,
    now: Instant,
) {
    let queue_session = (!request.purpose.is_auxiliary()).then(|| request.session_id.clone());
    if let Some(session) = &queue_session {
        processed_request_ids.retain(|request_id, mark| {
            request_id == &request.request_id || mark.queue_session.as_ref() != Some(session)
        });
    }
    processed_request_ids.insert(
        request.request_id.clone(),
        ProcessedMark {
            at: now,
            queue_session,
        },
    );
}

pub(super) fn take_next_eligible_pending_request(
    processed_request_ids: &mut ProcessedRequests,
    requests: Vec<AgentRequest>,
    now: Instant,
) -> Option<AgentRequest> {
    for request in requests {
        if request_is_cooling_down(processed_request_ids, &request.request_id, now) {
            continue;
        }

        mark_processed(processed_request_ids, &request, now);
        return Some(request);
    }

    None
}
