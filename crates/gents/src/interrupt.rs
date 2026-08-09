//! Shared types and helpers for request interruption signaling.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;
use tokio::sync::watch;

use crate::graphql::escape_graphql_string;
use crate::lifecycle::queue::{drain_automated_wakeups, drain_subagent_owned_queue};

#[derive(Debug, Clone, Deserialize)]
struct InterruptRequestRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    request_id: String,
    session_id: Option<String>,
    agent_did: Option<String>,
    interrupt_requested_at: Option<String>,
}

fn resolve_interrupt_request(
    request_id: &str,
    rows: Vec<InterruptRequestRow>,
) -> Result<Option<InterruptRequestRow>> {
    if let Some(row) = rows.iter().find(|row| row.request_id != request_id) {
        bail!(
            "AgentRequest logical key mismatch: queried request_id={request_id} but _docID={} returned request_id={}",
            row.doc_id,
            row.request_id
        );
    }
    Ok(crate::session::resolve_exact_logical_match(
        "AgentRequest",
        "request_id",
        request_id,
        rows,
        |row| row.doc_id.as_str(),
    )?)
}

fn interrupt_mutation(doc_id: &str, interrupt_requested_at: &str) -> String {
    let escaped_doc_id = escape_graphql_string(doc_id);
    let escaped_interrupt_requested_at = escape_graphql_string(interrupt_requested_at);
    format!(
        r#"mutation {{
            update_AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }},
                input: {{ interrupt_requested_at: "{escaped_interrupt_requested_at}" }}
            ) {{ _docID }}
        }}"#
    )
}

async fn load_interrupt_request(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<InterruptRequestRow>> {
    let escaped_request_id = escape_graphql_string(request_id);
    let lookup_query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ request_id: {{ _eq: "{escaped_request_id}" }} }}
            ) {{
                _docID
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
            "query interrupt target request_id={request_id} failed: {}",
            lookup
                .errors
                .iter()
                .map(|error| error.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    let rows: Vec<InterruptRequestRow> = lookup
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .map(|value| serde_json::from_value(value.clone()))
        .transpose()
        .context("decoding complete AgentRequest interrupt target set")?
        .unwrap_or_default();
    resolve_interrupt_request(request_id, rows)
}

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
    let Some(row) = load_interrupt_request(node, request_id).await? else {
        bail!("interrupt_request: request {request_id} not found");
    };
    let already_latched = row
        .interrupt_requested_at
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if already_latched {
        drain_request_queue_after_interrupt(node, request_id, &row).await;
        return Ok(());
    }

    let now = Utc::now().to_rfc3339();
    let mutation = interrupt_mutation(&row.doc_id, &now);
    // The latch mutation is idempotent and can race the source-spawn observer
    // or another interrupt caller. DefraDB reports those overlapping commits
    // as transient transaction conflicts, so use the runtime's bounded retry
    // seam instead of surfacing a flaky operator failure.
    let resp = crate::retry::execute_graphql_with_conflict_retry(
        node,
        &mutation,
        "latch AgentRequest interrupt_requested_at",
    )
    .await;
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
    // The lookup bound one physical document, so the mutation must report that
    // same document. A disappearing or substituted target is not an
    // idempotent success: the interrupt was not durably latched where intended.
    let updated_doc_ids = resp
        .data
        .as_ref()
        .and_then(|d| d.get("update_AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.get("_docID").and_then(|value| value.as_str()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if updated_doc_ids.as_slice() != [row.doc_id.as_str()] {
        bail!(
            "interrupt_request({request_id}) exact mutation expected _docID={} but updated _docIDs={updated_doc_ids:?}",
            row.doc_id
        );
    }
    drain_request_queue_after_interrupt(node, request_id, &row).await;
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
                    status: {{ _eq: "processing" }},
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
    row: &InterruptRequestRow,
) {
    let Some(session_id) = row
        .session_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    else {
        tracing::warn!(
            request_id = %request_id,
            "interrupted request has no session_id; cannot drain automated wake-ups"
        );
        return;
    };

    let Some(agent_did) = row
        .agent_did
        .as_deref()
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
    Ok(load_interrupt_request(node, request_id)
        .await?
        .and_then(|row| row.interrupt_requested_at)
        .filter(|value| !value.trim().is_empty()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn interrupt_row(doc_id: &str, request_id: &str) -> InterruptRequestRow {
        InterruptRequestRow {
            doc_id: doc_id.to_string(),
            request_id: request_id.to_string(),
            session_id: Some("session-one".to_string()),
            agent_did: Some("did:key:agent".to_string()),
            interrupt_requested_at: None,
        }
    }

    #[test]
    fn interrupt_resolution_rejects_logical_twins_deterministically() {
        let error = resolve_interrupt_request(
            "request-same",
            vec![
                interrupt_row("doc-z", "request-same"),
                interrupt_row("doc-a", "request-same"),
            ],
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "AgentRequest logical identity conflict for request_id=request-same: _docIDs=[\"doc-a\", \"doc-z\"]"
        );
    }

    #[test]
    fn interrupt_mutation_targets_only_the_bound_document() {
        let mutation = interrupt_mutation("doc-\"hostile", "2026-08-08T12:00:00Z");

        assert!(mutation.contains(r#"filter: { _docID: { _eq: "doc-\"hostile" } }"#));
        assert!(!mutation.contains("request_id"));
    }

    #[tokio::test]
    async fn interrupt_request_rejects_physical_twins_before_mutation() {
        let tempdir = TempDir::new().unwrap();
        let node = EmbeddedNode::builder()
            .data_path(tempdir.path())
            .build()
            .await
            .unwrap();
        crate::schema::ensure_runtime_schemas(&node).await.unwrap();
        let mutation = r#"mutation {
            first: create_AgentRequest(input: {
                request_id: "request-twin",
                agent_did: "did:test:agent",
                session_id: "session-twin",
                content: "first physical document",
                status: "processing",
                lifecycle_state: "processing"
            }) { _docID }
            second: create_AgentRequest(input: {
                request_id: "request-twin",
                agent_did: "did:test:agent",
                session_id: "session-twin",
                content: "second physical document",
                status: "processing",
                lifecycle_state: "processing"
            }) { _docID }
        }"#;
        let response = node.execute(mutation).await;
        assert!(
            !response.has_errors(),
            "fixture create: {:?}",
            response.errors
        );

        let error = interrupt_request(&node, "request-twin").await.unwrap_err();
        let message = error.to_string();
        assert!(message.contains("AgentRequest logical identity conflict"));
        assert!(message.contains("request_id=request-twin"));

        let query = r#"{
            AgentRequest(filter: { request_id: { _eq: "request-twin" } }) {
                interrupt_requested_at
            }
        }"#;
        let response = node.execute(query).await;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .and_then(|value| value.as_array())
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row
            .get("interrupt_requested_at")
            .is_none_or(serde_json::Value::is_null)));
    }
}
