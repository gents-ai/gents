use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::lifecycle::queue::drain_automated_wakeups_in_txn;

mod scoped;
pub use scoped::fetch_interrupt_requested_at_scoped;
pub(crate) use scoped::observe_interrupt_requested_at_scoped;

/// Requests a soft interrupt by latching `interrupt_requested_at` on the
/// AgentRequest document. Idempotent: the first latch drains automated wakes
/// visible as pending in the same transaction and exact session scope; a
/// replay preserves the existing timestamp and does not scan the queue.
///
/// Concurrent same-node lookup and latch run in the existing transaction
/// owner. In P2P-replicated deployments independent writers may each stamp and
/// CRDT merge picks one timestamp: audit meaning is preserved, microsecond
/// ordering is not.
pub async fn interrupt_request(node: &Arc<EmbeddedNode>, request_id: &str) -> Result<()> {
    let logical = escape_graphql_string(request_id);
    interrupt_request_matching(node, format!("request_id:{{_eq:\"{logical}\"}}"))
        .await
        .map(|_| ())
}

pub async fn interrupt_request_by_doc_id(
    node: &Arc<EmbeddedNode>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    interrupt_request_by_doc_id_returning_drained_wake_ids(
        node,
        request_doc_id,
        node_did,
        requester_did,
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
pub(crate) async fn interrupt_if_active_by_doc_id(
    node: &Arc<EmbeddedNode>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "interrupt.active_request",
        |txn| {
            Box::pin(async move {
                interrupt_if_active_in_txn(txn, request_doc_id, node_did, requester_did).await
            })
        },
    )
    .await
}

pub(crate) async fn interrupt_if_active_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    let filter = exact_request_filter(request_doc_id, node_did, requester_did)?;
    let response = txn.execute(&format!(r#"{{ AgentRequest(filter: {{ {filter}, lifecycle_state: {{ _in: ["claimed", "processing"] }} }}, limit: 2) {{ _docID }} }}"#)).await?;
    let rows = response["data"]["AgentRequest"]
        .as_array()
        .context("active interrupt query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "active interrupt request is ambiguous");
    if rows.is_empty() {
        return Ok(false);
    }
    interrupt_request_matching_in_txn(txn, &filter).await?;
    Ok(true)
}

pub(crate) async fn interrupt_request_by_doc_id_returning_drained_wake_ids(
    node: &Arc<EmbeddedNode>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<String>> {
    interrupt_request_matching(
        node,
        exact_request_filter(request_doc_id, node_did, requester_did)?,
    )
    .await
}

fn exact_request_filter(
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty() && !node_did.trim().is_empty(),
        "interrupt requires physical request and node identity"
    );
    let physical = escape_graphql_string(request_doc_id);
    let owner = escape_graphql_string(node_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "_docID:{{_eq:\"{physical}\"}},node_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

fn scoped_request_filter(
    request_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_id.trim().is_empty() && !node_did.trim().is_empty(),
        "interrupt fetch requires logical request and node identity"
    );
    let logical = escape_graphql_string(request_id);
    let owner = escape_graphql_string(node_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "request_id:{{_eq:\"{logical}\"}},node_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

pub async fn interrupt_request_by_doc_id_with_access(
    access: &crate::config_client::ConfigAccess,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let filter = exact_request_filter(request_doc_id, node_did, requester_did)?;
    access
        .transact_write("interrupt.latch_request", |txn| {
            let filter = &filter;
            Box::pin(async move { interrupt_request_matching_in_txn(txn, filter).await })
        })
        .await
        .map(|_| ())
}

/// Latch the exact scoped request inside the caller's transaction, so a job's
/// terminal decision and child interrupts share one commit and rollback boundary.
pub(crate) async fn interrupt_request_by_doc_id_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let filter = exact_request_filter(request_doc_id, node_did, requester_did)?;
    interrupt_request_matching_in_txn(txn, &filter)
        .await
        .map(|_| ())
}

async fn interrupt_request_matching(
    node: &std::sync::Arc<EmbeddedNode>,
    filter: String,
) -> Result<Vec<String>> {
    crate::config_client::ConfigAccess::transact_local_write(
        node,
        None,
        "interrupt.latch_request",
        |txn| {
            let filter = &filter;
            Box::pin(async move { interrupt_request_matching_in_txn(txn, filter).await })
        },
    )
    .await
}

async fn interrupt_request_matching_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    filter: &str,
) -> Result<Vec<String>> {
    let lookup = txn.execute(&format!(r#"{{AgentRequest(filter: {{{filter}}}, limit: 2) {{_docID request_id session_id node_did requester_did interrupt_requested_at}}}}"#)).await?;
    let rows = lookup["data"]["AgentRequest"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("interrupt request query omitted rows"))?;
    anyhow::ensure!(
        rows.len() == 1,
        "interrupt request is missing or ambiguous within selected scope"
    );
    let row = rows[0].clone();
    anyhow::ensure!(
        row["request_id"].as_str().is_some(),
        "interrupt request missing logical identity"
    );
    let physical = row["_docID"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("interrupt request missing physical identity"))?;
    let session_id = row["session_id"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("interrupt request missing session identity"))?;
    let node_did = row["node_did"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("interrupt request missing node identity"))?;
    crate::goal::pause_goal_for_interrupt_in_txn(txn, node_did, session_id).await?;
    if row["interrupt_requested_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(Vec::new());
    }
    let physical = escape_graphql_string(physical);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let result = txn.execute(&format!(r#"mutation {{update_AgentRequest(docID: "{physical}", filter: {{_docID: {{_eq: "{physical}"}}}}, input: {{interrupt_requested_at: "{now}"}}) {{_docID}}}}"#)).await?;
    let updated = result["data"]["update_AgentRequest"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("interrupt mutation omitted affected rows"))?;
    anyhow::ensure!(
        updated.len() == 1 && updated[0]["_docID"] == row["_docID"],
        "interrupt mutation did not update the selected physical request"
    );
    drain_automated_wakeups_in_txn(
        txn,
        session_id,
        node_did,
        row["requester_did"].as_str(),
        "automated wake-up drained because active request was interrupted",
    )
    .await
}

pub(crate) async fn interrupt_active_session_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ {scope}, purpose: {{ _eq: "normal" }},
                lifecycle_state: {{ _in: ["claimed", "processing"] }} }}, limit: 2)
                {{ _docID request_id }} }}"#
        ))
        .await?;
    let rows = response
        .pointer("/data/AgentRequest")
        .and_then(serde_json::Value::as_array)
        .context("active session request query omitted rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active requests in exact session scope"
    );
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let doc_id = row["_docID"]
        .as_str()
        .context("active session request omitted _docID")?;
    let request_id = row["request_id"]
        .as_str()
        .context("active session request omitted request_id")?
        .to_owned();
    let filter = exact_request_filter(doc_id, node_did, requester_did)?;
    interrupt_request_matching_in_txn(txn, &filter).await?;
    Ok(Some(request_id))
}

pub(crate) async fn active_session_request(
    node: &EmbeddedNode,
    session_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::row::AgentRequestRow>> {
    let scope = crate::session::session_scope_filter(node_did, session_id, requester_did);
    let response = graphql_with_transaction_retry(
        node,
        &format!(r#"{{AgentRequest(filter:{{{scope},purpose:{{_eq:"normal"}},lifecycle_state:{{_in:["claimed","processing"]}}}},limit:2){{_docID request_id node_did requester_did session_id lifecycle_state}}}}"#),
        "active request lookup",
    )
    .await?;
    anyhow::ensure!(
        response
            .data
            .as_ref()
            .and_then(|data| data.get("AgentRequest"))
            .is_some(),
        "active request query omitted rows"
    );
    let mut rows: Vec<gents_protocol::row::AgentRequestRow> =
        crate::graphql::rows(&response, "AgentRequest")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "multiple active physical requests in exact session scope"
    );
    Ok(rows.pop())
}

pub async fn fetch_interrupt_requested_at_by_doc_id(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Option<String>> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "interrupt fetch requires a physical request document id"
    );
    let escaped = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{escaped}" }} }},
                limit: 2
            ) {{
                interrupt_requested_at
            }}
        }}"#
    );
    let resp =
        graphql_with_transaction_retry(node, &query, "fetch_interrupt_requested_at_by_doc_id")
            .await
            .with_context(|| format!("fetch_interrupt_requested_at_by_doc_id({request_doc_id})"))?;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("interrupt request fetch omitted rows"))?;
    anyhow::ensure!(rows.len() <= 1, "interrupt request fetch is ambiguous");
    let value = rows
        .first()
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    Ok(value)
}

pub async fn fetch_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let request_doc_id = crate::request_binding::require_request_doc_id(node, request_id).await?;
    fetch_interrupt_requested_at_by_doc_id(node, &request_doc_id).await
}

#[derive(Debug, Clone)]
pub struct InterruptIntent {
    pub at: DateTime<Utc>,
}

const OBSERVER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn an observer task that reads `interrupt_requested_at` for a single
/// request and signals the channel when the field flips to non-null. The field
/// is re-read on document changes (local write or replicated merge) and on a
/// slower fallback tick in case the change subscription overflows or closes.
/// Exits when signaled once, the shutdown receiver changes, or the handle is
/// aborted.
pub fn spawn_request_interrupt_observer(
    node: Arc<EmbeddedNode>,
    request_doc_id: String,
    interrupt_tx: watch::Sender<Option<InterruptIntent>>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let changes = crate::subscribe_collections(&node, ["AgentRequest"])
        .inspect_err(|error| {
            tracing::warn!(doc_id = %request_doc_id, %error, "interrupt observer polling only");
        })
        .ok();
    tokio::spawn(async move {
        observe_request_interrupt(
            &request_doc_id,
            changes,
            || read_interrupt_requested_at(&node, &request_doc_id),
            &interrupt_tx,
            &mut shutdown,
        )
        .await;
    })
}

trait InterruptChangeFeed: Send {
    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<events::DocumentChangeBatch>> + Send;
}

impl InterruptChangeFeed for events::DocumentChangeSubscription {
    fn recv(
        &mut self,
    ) -> impl std::future::Future<Output = Option<events::DocumentChangeBatch>> + Send {
        events::DocumentChangeSubscription::recv(self)
    }
}

async fn read_interrupt_requested_at(node: &EmbeddedNode, request_doc_id: &str) -> Option<String> {
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                interrupt_requested_at
            }}
        }}"#,
        doc_id = escape_graphql_string(request_doc_id),
    );
    let resp = match graphql_with_transaction_retry(node, &query, "interrupt observer query").await
    {
        Ok(resp) => resp,
        Err(error) => {
            tracing::warn!(
                doc_id = %request_doc_id,
                error = %error,
                "interrupt observer query failed; will retry"
            );
            return None;
        }
    };
    resp.data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|arr| arr.first())
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The fallback tick is polled before the change feed so a feed that is
/// always ready with unrelated documents cannot starve the initial read or
/// the periodic re-read.
async fn observe_request_interrupt<F, R, Fut>(
    request_doc_id: &str,
    mut changes: Option<F>,
    mut read: R,
    interrupt_tx: &watch::Sender<Option<InterruptIntent>>,
    shutdown: &mut watch::Receiver<bool>,
) where
    F: InterruptChangeFeed,
    R: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<String>>,
{
    let mut ticker = tokio::time::interval(OBSERVER_POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => return,
            _ = ticker.tick() => {}
            batch = async {
                match changes.as_mut() {
                    Some(changes) => changes.recv().await,
                    None => std::future::pending().await,
                }
            } => match batch {
                Some(batch)
                    if batch.resync_required
                        || batch
                            .changes
                            .iter()
                            .any(|change| change.doc_id == request_doc_id) => {}
                Some(_) => continue,
                None => {
                    tracing::warn!(
                        doc_id = %request_doc_id,
                        "interrupt observer change subscription closed; polling only"
                    );
                    changes = None;
                    continue;
                }
            },
        }
        if interrupt_tx.borrow().is_some() {
            return;
        }
        let Some(at_str) = read().await else {
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
            }
        }
    }
}

#[cfg(test)]
mod observer_scheduling_tests;

#[cfg(test)]
mod scope_tests;

#[cfg(test)]
mod physical_scope_tests;

#[cfg(test)]
mod queue_tests;
