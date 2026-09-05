//! Shared types and helpers for request interruption signaling.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::{drain_automated_wakeups, drain_subagent_owned_queue};

/// Request a soft interrupt by latching `interrupt_requested_at` on the
/// AgentRequest document. Idempotent: if the field is already set, the
/// current timestamp is preserved and this call is a no-op.
///
/// The runtime's per-request observer (see `spawn_request_interrupt_observer`)
/// polls this field and signals the daemon to cancel in-flight inference and
/// transition the request to `interrupted`. Writing this field on a terminal
/// request is harmless — the lifecycle state machine filters terminal statuses.
///
/// # Concurrent callers
///
/// Under two concurrent `interrupt_request` callers both observing an empty
/// field, both will write; the last mutation wins. The latched value under
/// contention is therefore "interrupt requested near T" rather than "at
/// exactly T" — acceptable for audit semantics but weaker than a strict
/// first-writer-wins contract. S7 (`interrupt_monotonicity`) holds on the
/// ideal state machine as-stated (the field is never unset once set); the
/// physical race only affects which timestamp gets persisted, not whether
/// a timestamp is persisted.
///
/// In P2P-replicated deployments, independent writers on different nodes
/// may each stamp, and CRDT merge will pick whichever timestamp sorts
/// higher by DefraDB's LWW rules. Same conclusion: audit meaning is
/// preserved; microsecond-exact ordering is not.
pub async fn interrupt_request(node: &EmbeddedNode, request_id: &str) -> Result<()> {
    // Combined existence + latch-status check. We distinguish "no row" from
    // "row with empty field" so that interrupting a bogus request id reports
    // an error instead of silently succeeding with a no-op mutation.
    //
    // Pre-check is also an optimization: the submitter latches on first write,
    // and subsequent writers must not clobber the timestamp. DefraDB's update
    // mutation does not have an atomic "set-if-null" so we read-then-write.
    let escaped_request_id = escape_graphql_string(request_id);
    let lookup_query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                limit: 1
            ) {{
                request_id
                session_id
                agent_did
                interrupt_requested_at
            }}
        }}"#
    );
    let lookup = node.execute(&lookup_query).await;
    if lookup.has_errors() {
        bail!(
            "interrupt_request({request_id}) lookup failed: {}",
            lookup
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let row = lookup
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first());
    let Some(row) = row else {
        bail!("interrupt_request: request {request_id} not found");
    };
    let already_latched = row
        .get("interrupt_requested_at")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty());
    if already_latched {
        drain_request_queue_after_interrupt(node, request_id, row).await;
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let escaped_now = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );
    // The latch mutation is idempotent and can race the source-spawn observer
    // or another interrupt caller. DefraDB reports those overlapping commits
    // as transient transaction conflicts, so use the runtime's bounded retry
    // seam instead of surfacing a flaky operator failure.
    let resp = crate::graphql::graphql_mutation_with_transaction_retry(
        node,
        &mutation,
        "latch AgentRequest interrupt_requested_at",
    )
    .await?;
    // Defensive: confirm at least one row was updated. Zero rows would mean
    // either the row was deleted between lookup and mutation, or another
    // writer raced us (idempotent). Treat as success, log for observability.
    let updated = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|arr| arr.len())
        .unwrap_or(0);
    if updated == 0 {
        tracing::info!(
            request_id = %request_id,
            "interrupt_request mutation updated 0 rows; treating as idempotent (racy delete or concurrent latch)"
        );
    }
    drain_request_queue_after_interrupt(node, request_id, row).await;
    Ok(())
}

pub(crate) async fn interrupt_active_session_request(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<bool> {
    let Some(request_id) = active_session_request_id(node, session_id).await? else {
        return Ok(false);
    };
    interrupt_request(node, &request_id).await?;
    Ok(true)
}

pub(crate) async fn cancel_subagent_session_queue(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    reason: &str,
) -> Result<usize> {
    drain_subagent_owned_queue(node, session_id, agent_did, reason).await
}

async fn active_session_request_id(
    node: &EmbeddedNode,
    session_id: &str,
) -> Result<Option<String>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{
                    session_id: {{ _eq: "{escaped_session_id}" }},
                    lifecycle_state: {{ _in: ["claimed", "processing"] }}
                }},
                order: [{{ created_at: ASC }}, {{ request_id: ASC }}],
                limit: 1
            ) {{
                request_id
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    if response.has_errors() {
        bail!(
            "query active request for session {session_id} failed: {}",
            response
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }

    Ok(response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(|value| value.as_array())
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("request_id"))
        .and_then(|value| value.as_str())
        .map(str::to_owned))
}

async fn drain_request_queue_after_interrupt(
    node: &EmbeddedNode,
    request_id: &str,
    row: &serde_json::Value,
) {
    let Some(session_id) = row
        .get("session_id")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        tracing::warn!(
            request_id = %request_id,
            "interrupted request has no session_id; cannot drain automated wake-ups"
        );
        return;
    };

    let Some(agent_did) = row
        .get("agent_did")
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
    else {
        tracing::warn!(
            request_id = %request_id,
            session_id = %session_id,
            "interrupted request has no agent_did; cannot drain automated wake-ups"
        );
        return;
    };

    let drained = match drain_automated_wakeups(
        node,
        session_id,
        agent_did,
        "automated wake-up drained because active request was interrupted",
    )
    .await
    {
        Ok(drained) => drained,
        Err(error) => {
            tracing::warn!(
                request_id = %request_id,
                session_id = %session_id,
                error = %error,
                "failed to drain queued automated wake-ups after request interrupt"
            );
            return;
        }
    };
    if drained > 0 {
        tracing::info!(
            request_id = %request_id,
            session_id = %session_id,
            drained,
            "drained queued automated wake-ups after request interrupt"
        );
    }
}

pub async fn fetch_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
            }}
        }}"#
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        bail!(
            "fetch_interrupt_requested_at({request_id}) failed: {}",
            resp.errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let value = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(value)
}

#[derive(Debug, Clone)]
pub struct InterruptIntent {
    pub at: DateTime<Utc>,
}

const OBSERVER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn an observer task that polls `interrupt_requested_at` for a single
/// request and signals the channel when the field flips to non-null.
///
/// Polls rather than subscribes because DefraDB lacks per-field watchpoints;
/// the 2s interval is a compromise between Esc UX latency and DB load.
///
/// The task exits when:
///   - the channel has been signaled once (idempotent latch), OR
///   - the shutdown receiver changes, OR
///   - the returned `JoinHandle` is aborted.
pub fn spawn_request_interrupt_observer(
    node: Arc<EmbeddedNode>,
    request_doc_id: String,
    interrupt_tx: watch::Sender<Option<InterruptIntent>>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(OBSERVER_POLL_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = ticker.tick() => {}
            }
            if interrupt_tx.borrow().is_some() {
                return;
            }
            let query = format!(
                r#"query {{
                    AgentRequest(
                        filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                        limit: 1
                    ) {{
                        interrupt_requested_at
                    }}
                }}"#,
                doc_id = escape_graphql_string(&request_doc_id),
            );
            let resp = node.execute(&query).await;
            if resp.has_errors() {
                tracing::warn!(
                    doc_id = %request_doc_id,
                    errors = ?resp.errors,
                    "interrupt observer query failed; will retry"
                );
                continue;
            }
            let Some(at_str) = resp
                .data
                .as_ref()
                .and_then(|d| d.get("AgentRequest"))
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|row| row.get("interrupt_requested_at"))
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
            else {
                continue;
            };
            match chrono::DateTime::parse_from_rfc3339(&at_str) {
                Ok(dt) => {
                    let intent = InterruptIntent {
                        at: dt.with_timezone(&Utc),
                    };
                    let _ = interrupt_tx.send(Some(intent));
                    tracing::info!(
                        request_doc_id = %request_doc_id,
                        interrupt_at = %dt.to_rfc3339(),
                        "interrupt observer latched; signaled daemon"
                    );
                    return;
                }
                Err(e) => {
                    tracing::warn!(
                        doc_id = %request_doc_id,
                        bad_value = %at_str,
                        error = %e,
                        "invalid interrupt_requested_at; observer continuing"
                    );
                    continue;
                }
            }
        }
    })
}
