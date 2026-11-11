use super::claim::{combine, ClaimVerdict};
use super::native::TxnHost;
use super::prepared::{admit, inputs_digest, prepare, Binding, Stale};
use super::queue_scope::{QueueScopes, SessionRequests};
use super::registry::{
    AdmitShown, ClaimApplied, ClaimCandidate, QueueScopeAsk, Registry, RequestTerminal,
    SessionHalt,
};
use super::stage::{gate, stage_dropping, stage_naming, HookError, Staged};
use crate::request::admission::{
    load_exact_parent_request, required_row_string, AdmissionResult, AgentRequestAdmissionError,
};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_store::config_client::ConfigApplyTxn;

/// A store or core-operation failure reaches the caller as the error it was
/// before the seam, so its text and its conflict classification are unchanged.
fn surface(error: HookError) -> anyhow::Error {
    match error {
        HookError::Store(error) => error.into(),
        HookError::Operation(error) | HookError::Revisions(error) | HookError::CoreRead(error) => {
            error
        }
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

/// `request.claim.applied` (may refuse). Called after core's claim affected
/// the request's row, in the same transaction; a refusal aborts the claim with
/// the plugin's reason. Core reads the request's Task fire receipt (Triggers
/// are core) and shows it in the event, so a plugin that needs nothing for a
/// request without one declares no reads.
pub async fn claim_applied(
    txn: &ConfigApplyTxn<'_>,
    request: &gents_protocol::row::AgentRequest,
    claimed_at: &str,
) -> anyhow::Result<()> {
    let registry = Registry::for_transaction(txn)?;
    claim_applied_in(txn, &registry, request, claimed_at).await
}

/// `claim_applied` against an explicit registry.
pub async fn claim_applied_in(
    txn: &ConfigApplyTxn<'_>,
    registry: &Registry,
    request: &gents_protocol::row::AgentRequest,
    claimed_at: &str,
) -> anyhow::Result<()> {
    use gents_protocol::document_query::{fields::trigger_fire as fields, Filter, Query, Selection};
    let response = txn
        .run_local_response(
            &Query::select(
                Selection::doc_id()
                    .field(fields::SESSION_ID)
                    .field(fields::GOAL_ID)
                    .field(fields::GOAL_OBJECTIVE)
                    .field(fields::GOAL_TOKEN_BUDGET)
                    .field(fields::GOAL_ASSIGNMENT_APPLIED),
            )
                .filter(Filter::eq(fields::OWNER_DID, &request.node_did))
                .filter(Filter::eq(fields::REQUEST_ID, &request.request_id))
                .render(),
        )
        .await?;
    let mut fires: Vec<serde_json::Value> =
        gents_store::graphql::rows(&response, "TriggerFire")?;
    anyhow::ensure!(fires.len() <= 1, "request has multiple Task fire receipts");
    let event = ClaimApplied {
        request: request.clone(),
        fire: fires.pop(),
        actor: txn.actor_did().map(str::to_owned),
        at: claimed_at.to_owned(),
    };
    let host = TxnHost::new(txn, registry);
    stage_naming(
        &host,
        &registry.claim_applied,
        &event,
        std::slice::from_ref(&request.agent_did),
    )
    .await
    .map_err(surface)?;
    Ok(())
}

/// `request.admit.verify` (may refuse): each prepared-decision plugin that
/// claims the request from the row alone is shown the request and its exact
/// parent, read as the request's agent, and its verdict is re-validated
/// (Lean `Hooks.admit`). A request no plugin claims is core's own decision and
/// reads nothing more; a claiming plugin admits only with a matching `Allow`.
pub(crate) async fn request_admit_verify(
    node: &std::sync::Arc<defra_node::Node>,
    request: &AgentRequestRow,
    agent: &str,
) -> AdmissionResult<()> {
    let registry = Registry::current().map_err(AgentRequestAdmissionError::unavailable)?;
    let claimants = registry
        .request_admit_verify
        .iter()
        .filter(|plugin| plugin.applies(request))
        .collect::<Vec<_>>();
    if claimants.is_empty() {
        return Ok(());
    }
    let parent = load_exact_parent_request(
        node,
        required_row_string(
            request.caused_by_parent_request_doc_id.as_deref(),
            "parent document ID",
        )?,
        agent,
    )
    .await?;
    let shown = AdmitShown {
        request: request.clone(),
        parent,
    };
    let inputs = inputs_digest(&shown).map_err(AgentRequestAdmissionError::unavailable)?;
    for plugin in claimants {
        let binding = Binding {
            actor: agent.to_owned(),
            point: REQUEST_ADMIT_VERIFY,
            subject: request.doc_id.clone(),
            inputs: inputs.clone(),
            configuration: plugin.configuration().to_owned(),
        };
        let decision = prepare(binding.clone(), |shown| plugin.decide(shown), &shown);
        // Both callers reach this point only after core's own admission
        // observation admitted the request, and cancellation was never an
        // admission input. Admission reads once, so the current binding is the
        // one captured from that read.
        admit(true, false, &binding, Some(decision)).map_err(|stale| match stale {
            Stale::Denied(reason) => AgentRequestAdmissionError::denied(anyhow::anyhow!(reason)),
            stale @ (Stale::Unauthorized | Stale::Cancelled) => {
                AgentRequestAdmissionError::denied(stale)
            }
            stale @ (Stale::Missing | Stale::Stale | Stale::Unavailable) => {
                AgentRequestAdmissionError::unavailable(stale)
            }
        })?;
    }
    Ok(())
}

pub const REQUEST_ADMIT_VERIFY: &str = "request.admit.verify";

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
