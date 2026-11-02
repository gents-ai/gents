use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde_json::Value;

use super::evidence::session_requests;
use crate::defra_node::EmbeddedNode;
use crate::eval::runner::embedded::observe::{collect_request_evidence, RequestEvidence};
use crate::store::graphql::graphql_with_transaction_retry;

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
pub(super) async fn settle_home(node: &EmbeddedNode, budget: Option<Duration>) -> Result<bool> {
    let started = Instant::now();
    let mut quiet: Option<(Instant, usize)> = None;
    loop {
        let (total, running) = request_counts(node).await?;
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
async fn request_counts(node: &EmbeddedNode) -> Result<(usize, usize)> {
    let response = graphql_with_transaction_retry(
        node,
        "{ AgentRequest { lifecycle_state } }",
        "eval trial settle",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
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
    node: &Arc<EmbeddedNode>,
    session_id: &str,
    request_id: &str,
    evidence: &mut RequestEvidence,
) -> Result<()> {
    let requests = session_requests(node, session_id).await?;
    let later = requests
        .iter()
        .skip_while(|request| request.request_id != request_id)
        .skip(1);
    for request in later {
        let more = collect_request_evidence(node, &request.request_id).await?;
        evidence.messages.extend(more.messages);
        evidence.tool_calls.extend(more.tool_calls);
        evidence.inference_calls.extend(more.inference_calls);
    }
    Ok(())
}
