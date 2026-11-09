use std::time::{Duration, Instant};

use anyhow::Result;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use super::evidence::session_requests_with_access;
use crate::eval::runner::embedded::observe::RequestEvidence;
use crate::ConfigAccess;

/// How long every request must stay terminal before the home counts as quiet:
/// a trigger fire, a queued delivery or a goal continuation writes its request
/// a moment after the document that causes it.
const SETTLE_QUIET: Duration = Duration::from_secs(60);

const SETTLE_POLL: Duration = Duration::from_secs(2);

/// What is left of an opt-in stage deadline; absent means unbounded.
pub(super) fn remaining_of(deadline: Option<Duration>, started: Instant) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_sub(started.elapsed()))
}

/// Absent `budget` waits until the home is quiet.
pub(super) async fn settle_home(access: &ConfigAccess, budget: Option<Duration>) -> Result<bool> {
    let started = Instant::now();
    let mut quiet: Option<(Instant, usize)> = None;
    loop {
        let (total, running) = request_counts(access).await?;
        let now = Instant::now();
        quiet = match quiet {
            Some((since, seen)) if running == 0 && seen == total => Some((since, seen)),
            _ if running == 0 => Some((now, total)),
            _ => None,
        };
        if quiet.is_some_and(|(since, _)| now.duration_since(since) >= SETTLE_QUIET) {
            return Ok(true);
        }
        let left = remaining_of(budget, started);
        if left.is_some_and(|left| left.is_zero()) {
            return Ok(false);
        }
        tokio::time::sleep(left.map_or(SETTLE_POLL, |left| SETTLE_POLL.min(left))).await;
    }
}
async fn request_counts(access: &ConfigAccess) -> Result<(usize, usize)> {
    let response = access
        .execute("{ AgentRequest { lifecycle_state } }")
        .await?;
    let rows = response
        .get("data")
        .and_then(|data| data.get("AgentRequest"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let running = rows
        .iter()
        .filter(|row| {
            !row.get("lifecycle_state")
                .and_then(Value::as_str)
                .and_then(|state| RequestLifecycleState::parse(state).ok())
                .is_some_and(|state| state.is_terminal())
        })
        .count();
    Ok((rows.len(), running))
}

pub(super) async fn extend_with_later_requests(
    access: &ConfigAccess,
    session_id: &str,
    request_id: &str,
    evidence: &mut RequestEvidence,
) -> Result<()> {
    let requests = session_requests_with_access(access, session_id).await?;
    let later = requests
        .iter()
        .skip_while(|request| request.request_id != request_id)
        .skip(1);
    for request in later {
        let more = crate::eval::runner::embedded::observe::collect_request_evidence_with_access(
            access,
            &request.request_id,
        )
        .await?;
        evidence.messages.extend(more.messages);
        evidence.tool_calls.extend(more.tool_calls);
        evidence.inference_calls.extend(more.inference_calls);
    }
    Ok(())
}
