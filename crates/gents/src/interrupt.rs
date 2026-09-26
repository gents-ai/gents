//! Shared types and helpers for request interruption signaling.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use tokio::sync::watch;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::lifecycle::queue::{drain_automated_wakeups_in_txn, drain_subagent_owned_queue};

/// Request a soft interrupt by latching `interrupt_requested_at` on the
/// AgentRequest document. Idempotent: if the field is already set, the
/// current timestamp is preserved and this call is a no-op.
/// The first latch drains automated wakes visible as pending in the same
/// transaction and exact session scope. A replay does not scan the queue.
///
/// The runtime's per-request observer (see `spawn_request_interrupt_observer`)
/// watches this field and signals the daemon to cancel in-flight inference and
/// transition the request to `interrupted`. A terminal target keeps its
/// lifecycle state, though its first latch still drains pending automated wakes.
///
/// # Concurrent callers
///
/// Same-node lookup and latch run in the existing transaction owner, preserving
/// an already observed timestamp. Distributed concurrent writers still follow
/// DefraDB's existing merge semantics.
///
/// In P2P-replicated deployments, independent writers on different nodes
/// may each stamp, and CRDT merge will pick whichever timestamp sorts
/// higher by DefraDB's LWW rules. Same conclusion: audit meaning is
/// preserved; microsecond-exact ordering is not.
pub async fn interrupt_request(node: &EmbeddedNode, request_id: &str) -> Result<()> {
    let logical = escape_graphql_string(request_id);
    interrupt_request_matching(node, format!("request_id:{{_eq:\"{logical}\"}}"))
        .await
        .map(|_| ())
}

/// Interrupt the exact request already selected within a principal/requester scope.
/// DefraDB ACP and the existing interruption owner retain all authorization and lifecycle work.
pub async fn interrupt_request_by_doc_id(
    node: &EmbeddedNode,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    interrupt_request_by_doc_id_returning_drained_wake_ids(
        node,
        request_doc_id,
        agent_did,
        requester_did,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn interrupt_request_by_doc_id_returning_drained_wake_ids(
    node: &EmbeddedNode,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Vec<String>> {
    interrupt_request_matching(
        node,
        exact_request_filter(request_doc_id, agent_did, requester_did)?,
    )
    .await
}

fn exact_request_filter(
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty() && !agent_did.trim().is_empty(),
        "interrupt requires physical request and principal identity"
    );
    let physical = escape_graphql_string(request_doc_id);
    let owner = escape_graphql_string(agent_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "_docID:{{_eq:\"{physical}\"}},agent_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

fn scoped_request_filter(
    request_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(
        !request_id.trim().is_empty() && !agent_did.trim().is_empty(),
        "interrupt fetch requires logical request and principal identity"
    );
    let logical = escape_graphql_string(request_id);
    let owner = escape_graphql_string(agent_did);
    let requester = requester_did
        .map(|did| format!("\"{}\"", escape_graphql_string(did)))
        .unwrap_or_else(|| "null".into());
    Ok(format!(
        "request_id:{{_eq:\"{logical}\"}},agent_did:{{_eq:\"{owner}\"}},requester_did:{{_eq:{requester}}}"
    ))
}

/// Latch the same exact interrupt through local or HTTP transaction access.
/// The existing completion loop observes the durable intent on the target node.
pub async fn interrupt_request_by_doc_id_with_access(
    access: &crate::config_client::ConfigAccess,
    request_doc_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<()> {
    let filter = exact_request_filter(request_doc_id, agent_did, requester_did)?;
    access
        .transact("interrupt.latch_request", |txn| {
            let filter = &filter;
            Box::pin(async move { interrupt_request_matching_in_txn(txn, filter).await })
        })
        .await
        .map(|_| ())
}

async fn interrupt_request_matching(node: &EmbeddedNode, filter: String) -> Result<Vec<String>> {
    crate::config_client::ConfigAccess::transact_local(
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
    let lookup = txn.execute(&format!(r#"{{AgentRequest(filter: {{{filter}}}, limit: 2) {{_docID request_id session_id agent_did requester_did interrupt_requested_at}}}}"#)).await?;
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
    let agent_did = row["agent_did"]
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("interrupt request missing principal identity"))?;
    if row["interrupt_requested_at"]
        .as_str()
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(Vec::new());
    }
    let physical = escape_graphql_string(physical);
    let now = escape_graphql_string(&Utc::now().to_rfc3339());
    let result = txn.execute(&format!(r#"mutation {{update_AgentRequest(filter: {{_docID: {{_eq: "{physical}"}}}}, input: {{interrupt_requested_at: "{now}"}}) {{_docID}}}}"#)).await?;
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
        agent_did,
        row["requester_did"].as_str(),
        "automated wake-up drained because active request was interrupted",
    )
    .await
}

pub(crate) async fn interrupt_active_session_request(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<bool> {
    let Some(row) = active_session_request(node, session_id, agent_did, requester_did).await?
    else {
        return Ok(false);
    };
    interrupt_request_by_doc_id(
        node,
        row.doc_id
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("active request has no physical identity"))?,
        agent_did,
        requester_did,
    )
    .await?;
    Ok(true)
}

pub(crate) async fn cancel_subagent_session_queue(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
    reason: &str,
) -> Result<usize> {
    drain_subagent_owned_queue(node, session_id, agent_did, requester_did, reason).await
}

pub(crate) async fn active_session_request(
    node: &EmbeddedNode,
    session_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<gents_protocol::row::AgentRequestRow>> {
    let scope = crate::session::session_scope_filter(agent_did, session_id, requester_did);
    let response = graphql_with_transaction_retry(
        node,
        &format!(r#"{{AgentRequest(filter:{{{scope},lifecycle_state:{{_in:["claimed","processing"]}}}},limit:2){{_docID request_id agent_did requester_did session_id}}}}"#),
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

/// Fetch the durable interrupt intent for the exact physical AgentRequest
/// document identified by `_docID`.
///
/// `_docID` is the globally unique physical key, so this lookup cannot
/// cross principals or collide on a shared logical `request_id`.
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

/// Fetch the durable interrupt intent by logical request id.
///
/// The logical id is not a physical key: resolution goes through the
/// existing request-binding owner, which rejects ambiguous logical ids
/// instead of silently reading one replica's row with `limit: 1`. Callers
/// carrying the physical `_docID` must prefer
/// `fetch_interrupt_requested_at_by_doc_id`.
pub async fn fetch_interrupt_requested_at(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<String>> {
    let request_doc_id = crate::request_binding::require_request_doc_id(node, request_id).await?;
    fetch_interrupt_requested_at_by_doc_id(node, &request_doc_id).await
}

/// Fetch the durable interrupt intent by logical request id within one
/// principal scope (`agent_did` + optional `requester_did`).
///
/// Cross-principal collisions on the same logical id are excluded by the
/// scope; any residual same-scope collision fails closed via the existing
/// ambiguity rejection (`limit: 2` + row-count check), never `limit: 1`.
pub async fn fetch_interrupt_requested_at_scoped(
    node: &EmbeddedNode,
    request_id: &str,
    agent_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    let filter = scoped_request_filter(request_id, agent_did, requester_did)?;
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{{filter}}},
                limit: 2
            ) {{
                interrupt_requested_at
            }}
        }}"#
    );
    let resp = graphql_with_transaction_retry(node, &query, "fetch_interrupt_requested_at_scoped")
        .await
        .with_context(|| format!("fetch_interrupt_requested_at_scoped({request_id})"))?;
    let rows = resp
        .data
        .as_ref()
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("interrupt request fetch omitted rows"))?;
    anyhow::ensure!(
        rows.len() <= 1,
        "interrupt request fetch is ambiguous within principal scope"
    );
    Ok(rows
        .first()
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from))
}

#[derive(Debug, Clone)]
pub struct InterruptIntent {
    pub at: DateTime<Utc>,
}

/// Holds a per-request interrupt observer for the lifetime of one claim. Every
/// path out of the claim — including the error returns before terminalization —
/// must stop the observer, and a bare `JoinHandle` drop leaves it polling the
/// node instead.
pub(crate) struct ClaimedRequestInterruptObserver(tokio::task::JoinHandle<()>);

impl Drop for ClaimedRequestInterruptObserver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl ClaimedRequestInterruptObserver {
    pub(crate) fn new(observer: tokio::task::JoinHandle<()>) -> Self {
        Self(observer)
    }
}

/// What a raced stretch of pre-provider preparation produced. `Interrupted`
/// carries no timestamp: `interrupt_requested_at` on the request row is the
/// durable one, and the caller's terminal write is stamped by its owner.
pub(crate) enum InterruptiblePreparation<T> {
    Ready(T),
    Interrupted,
}

/// Run `work` while watching the durable interrupt latch, yielding whichever
/// arrives first.
///
/// Observation only: the latch is never a terminal write here. The caller's
/// lifecycle owner decides the outcome, and dropping `work` is what stops the
/// preparation it was doing.
pub(crate) async fn prepare_unless_interrupted<T>(
    interrupt_rx: &mut watch::Receiver<Option<InterruptIntent>>,
    work: impl std::future::Future<Output = T>,
) -> InterruptiblePreparation<T> {
    if interrupt_rx.borrow_and_update().is_some() {
        return InterruptiblePreparation::Interrupted;
    }
    tokio::pin!(work);
    loop {
        // The latch is checked first so an interrupt that is already observable
        // wins over preparation that is also ready.
        tokio::select! {
            biased;
            changed = interrupt_rx.changed() => {
                if changed.is_err() {
                    // The observer stopped, so no later latch can be observed
                    // here. Preparation continues; a latch written from now on
                    // is left to the durable reads the owned loop performs
                    // around inference, and to recovery for ownerless work.
                    return InterruptiblePreparation::Ready(work.await);
                }
                if interrupt_rx.borrow_and_update().is_some() {
                    return InterruptiblePreparation::Interrupted;
                }
            }
            ready = &mut work => return InterruptiblePreparation::Ready(ready),
        }
    }
}

const OBSERVER_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Spawn an observer task that reads `interrupt_requested_at` for a single
/// request and signals the channel when the field flips to non-null.
///
/// The field is re-read whenever the node reports a change to this request
/// document (local write or replicated merge), and on a slower fallback tick
/// in case the change subscription overflows or closes.
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
    let changes = node.subscribe_document_changes();
    tokio::spawn(async move {
        observe_request_interrupt(
            &request_doc_id,
            Some(changes),
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
