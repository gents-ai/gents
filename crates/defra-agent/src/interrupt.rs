//! Shared types and helpers for request interruption signaling.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::escape_graphql_string;

/// Request a soft interrupt by latching `interrupt_requested_at` on the
/// AgentRequest document. Idempotent: if the field is already set, the
/// current timestamp is preserved and this call is a no-op.
///
/// The runtime's per-request observer (see `spawn_request_interrupt_observer`)
/// polls this field and signals the daemon to cancel in-flight inference and
/// transition the request to `interrupted`. Writing this field on a terminal
/// request is harmless — the lifecycle state machine filters terminal statuses.
pub async fn interrupt_request(node: &EmbeddedNode, request_id: &str) -> Result<()> {
    // Pre-check is an optimization: the submitter latches on first write, and
    // subsequent writers must not clobber the timestamp. DefraDB's update
    // mutation does not have an atomic "set-if-null" so we read-then-write.
    if fetch_interrupt_requested_at(node, request_id)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let escaped_request_id = escape_graphql_string(request_id);
    let escaped_now = escape_graphql_string(&now);
    let mutation = format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_now}" }}
            ) {{ _docID }}
        }}"#
    );
    let resp = node.execute(&mutation).await;
    if resp.has_errors() {
        bail!(
            "interrupt_request({request_id}) failed: {}",
            resp.errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    Ok(())
}

/// Read `interrupt_requested_at` for the given request. Returns `None` if the
/// field is empty/unset or the request does not exist.
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

/// Signal sent from the per-request observer to the daemon when the request's
/// `interrupt_requested_at` field flips from null to non-null.
#[derive(Debug, Clone)]
pub struct InterruptIntent {
    /// RFC3339 timestamp the submitter wrote to `interrupt_requested_at`.
    pub at: DateTime<Utc>,
}

/// Interval between interrupt-field polls by the per-request observer.
/// Short enough that "Esc feels instant" (2s), long enough to bound DB load.
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
        // Skip the immediate first tick; wait a full interval before the first poll.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = ticker.tick() => {}
            }
            // Only signal once — if interrupt_tx.borrow() is already Some, exit.
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
