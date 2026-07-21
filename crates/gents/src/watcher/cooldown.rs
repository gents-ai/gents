use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::AgentRequest;

const MAX_PROCESSED_IDS: usize = 10_000;
pub(super) const GOSSIP_FALLBACK_POLL: Duration = Duration::from_secs(30);
pub(super) const PROCESSED_REQUEST_COOLDOWN: Duration = Duration::from_secs(30);

pub(super) fn prune_processed_requests(
    processed_request_ids: &mut HashMap<String, Instant>,
    now: Instant,
) {
    processed_request_ids.retain(|_, processed_at| {
        now.saturating_duration_since(*processed_at) < PROCESSED_REQUEST_COOLDOWN
    });

    if processed_request_ids.len() > MAX_PROCESSED_IDS {
        tracing::info!(
            count = processed_request_ids.len(),
            "pruning processed request ID set"
        );
        processed_request_ids.clear();
    }
}

pub(super) fn request_is_cooling_down(
    processed_request_ids: &mut HashMap<String, Instant>,
    request_id: &str,
    now: Instant,
) -> bool {
    match processed_request_ids.get(request_id).copied() {
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

pub(super) fn mark_processed(
    processed_request_ids: &mut HashMap<String, Instant>,
    request_id: &str,
    now: Instant,
) {
    processed_request_ids.insert(request_id.to_string(), now);
}

pub(super) fn take_next_eligible_pending_request(
    processed_request_ids: &mut HashMap<String, Instant>,
    requests: Vec<AgentRequest>,
    now: Instant,
) -> Option<AgentRequest> {
    for request in requests {
        if request_is_cooling_down(processed_request_ids, &request.request_id, now) {
            continue;
        }

        mark_processed(processed_request_ids, &request.request_id, now);
        return Some(request);
    }

    None
}
