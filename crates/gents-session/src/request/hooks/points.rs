use super::claim::{combine, ClaimVerdict};
use super::native::TxnHost;
use super::queue_scope::{QueueScopes, SessionRequests};
use super::registry::{ClaimCandidate, QueueScopeAsk, Registry, RequestTerminal, SessionHalt};
use super::stage::{gate, stage_dropping, HookError, Staged};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::config_client::ConfigApplyTxn;

/// A store or core-operation failure reaches the caller as the error it was
/// before the seam, so its text and its conflict classification are unchanged.
fn surface(error: HookError) -> anyhow::Error {
    match error {
        HookError::Store(error) => error.into(),
        HookError::Operation(error) | HookError::Revisions(error) => error,
        other => other.into(),
    }
}

/// A dropped staging is owed as a delivery (Lean `Hooks.owed`). No durable
/// record of a generic owed delivery exists yet; until one does, a plugin
/// whose state can lag redrives it from durable state, and the drop is
/// reported here.
fn report_owed(point: &'static str, subject: &str, staged: &[Staged]) {
    for entry in staged {
        if let Staged::Owed { plugin, cause } = entry {
            tracing::warn!(
                point,
                plugin,
                subject,
                error = %gents_protocol::error_chain::ErrorChain(cause),
                "lifecycle plugin staging was dropped; its delivery is owed"
            );
        }
    }
}

/// `request.terminal.stage` (must commit). Called after core's terminal
/// compare-and-set affected the request's row, in the same transaction.
pub async fn request_terminal(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    request_id: &str,
    state: RequestLifecycleState,
    reason: &str,
    at: &str,
) -> anyhow::Result<()> {
    let event = RequestTerminal {
        node_did: node_did.to_owned(),
        request_id: request_id.to_owned(),
        state,
        reason: reason.to_owned(),
        at: at.to_owned(),
    };
    let registry = Registry::for_transaction(txn)?;
    let host = TxnHost::new(txn, &registry);
    let staged = stage_dropping(&host, &registry.request_terminal, &event)
        .await
        .map_err(surface)?;
    report_owed("request.terminal.stage", request_id, &staged);
    Ok(())
}

/// `control.session.halt` (must commit). Called in the transaction that
/// writes an interrupt or stop latch.
pub async fn session_halt(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> anyhow::Result<()> {
    let event = SessionHalt {
        node_did: node_did.to_owned(),
        session_id: session_id.to_owned(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let registry = Registry::for_transaction(txn)?;
    let host = TxnHost::new(txn, &registry);
    let staged = stage_dropping(&host, &registry.session_halt, &event)
        .await
        .map_err(surface)?;
    report_owed("control.session.halt", session_id, &staged);
    Ok(())
}

/// `request.claim.gate` (may refuse): the combined verdict of every plugin.
pub async fn claim_gate(
    txn: &ConfigApplyTxn<'_>,
    event: &ClaimCandidate,
) -> anyhow::Result<ClaimVerdict> {
    let registry = Registry::for_transaction(txn)?;
    let host = TxnHost::new(txn, &registry);
    Ok(combine(
        gate(&host, &registry.claim_gate, event)
            .await
            .map_err(surface)?,
    ))
}

/// `request.queue.scope` (may refuse): core reads the session's request rows
/// and asks every plugin for the queue partition of the requests in `asked`.
/// A failure to answer one request is reported when the caller consults that
/// request.
pub async fn queue_scope(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    asked: Vec<String>,
) -> anyhow::Result<QueueScopes> {
    let requests = std::sync::Arc::new(SessionRequests::load(txn, node_did, session_id).await?);
    let event = QueueScopeAsk {
        node_did: node_did.to_owned(),
        session_id: session_id.to_owned(),
        requests: std::sync::Arc::clone(&requests),
        asked,
    };
    let registry = Registry::for_transaction(txn)?;
    let host = TxnHost::new(txn, &registry);
    let answers = gate(&host, &registry.queue_scope, &event)
        .await
        .map_err(surface)?;
    let named = registry
        .queue_scope
        .iter()
        .map(|plugin| plugin.manifest().name)
        .zip(answers)
        .collect();
    Ok(QueueScopes::new(requests, &event.asked, named))
}
