//! Shared types and helpers for request interruption signaling.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::escape_graphql_string;

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
