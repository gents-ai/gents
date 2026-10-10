use super::*;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

impl RequestLifecycle {
    pub(super) async fn request_view(&self) -> Result<Option<AgentRequestRow>> {
        request_view(&self.node, &self.request.doc_id).await
    }
}

pub(super) async fn request_view(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> Result<Option<AgentRequestRow>> {
    let doc_id = escape_graphql_string(request_doc_id);
    let query = format!(
        r#"{{
            AgentRequest(
                filter: {{ _docID: {{ _eq: "{doc_id}" }} }},
                limit: 1
            ) {{
                request_id
                lifecycle_state
                backend_id
                execution_generation
                execution_lease_expires_at
                execution_origin
                failure_reason
                terminal_output
            }}
        }}"#,
    );

    let resp = crate::graphql::graphql_with_transaction_retry(node, &query, "request status query")
        .await?;

    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&resp, "AgentRequest")?;

    Ok(rows.into_iter().next())
}

/// ConfigAccess holds the node's mutation gate from transaction creation through
/// commit. This read and the winning request CAS must stay inside that boundary.
pub(super) async fn claim_queue_allows(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    request: &AgentRequest,
) -> Result<bool> {
    let owner = escape_graphql_string(&request.node_did);
    let nonterminal = RequestLifecycleState::graphql_list(
        RequestLifecycleState::ALL
            .into_iter()
            .filter(|state| !state.is_terminal()),
    );
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{owner}" }},
            purpose: {{ _eq: "normal" }}, lifecycle_state: {{ _in: {nonterminal} }} }}) {{
            _docID request_id session_id lifecycle_state
        }} }}"#,
        ))
        .await?;
    let requests: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    let response = txn.execute_local_response(&format!(
        r#"{{ TriggerFire(filter: {{owner_did: {{_eq: "{owner}"}}}}) {{request_id trigger_id queued_serial}} }}"#
    )).await?;
    #[derive(serde::Deserialize)]
    struct QueueReceipt {
        request_id: String,
        trigger_id: String,
        queued_serial: bool,
    }
    let receipts: Vec<QueueReceipt> = crate::graphql::rows(&response, "TriggerFire")?;
    let by_request: std::collections::HashMap<_, _> = receipts
        .iter()
        .map(|receipt| (receipt.request_id.as_str(), receipt))
        .collect();
    let mut observations = requests
        .iter()
        .map(|row| {
            let receipt = by_request.get(row.request_id.as_str());
            crate::trigger_engine::durable::ClaimObservation {
                document: row.doc_id.clone().unwrap_or_default(),
                owner: request.node_did.clone(),
                session: row.session_id.clone().unwrap_or_default(),
                trigger: receipt.map(|r| r.trigger_id.clone()).unwrap_or_default(),
                serial: receipt.is_some_and(|r| r.queued_serial),
                receipt: receipt.is_some(),
                arrival: None,
                running: matches!(
                    row.lifecycle_state,
                    Some(RequestLifecycleState::Claimed | RequestLifecycleState::Processing)
                ),
                terminal: row
                    .lifecycle_state
                    .is_none_or(RequestLifecycleState::is_terminal),
            }
        })
        .collect::<Vec<_>>();
    let Some(candidate) = observations
        .iter()
        .find(|row| row.document == request.doc_id)
        .cloned()
    else {
        return Ok(false);
    };
    if !requests.iter().any(|row| {
        row.doc_id.as_deref() == Some(request.doc_id.as_str())
            && row.lifecycle_state == Some(RequestLifecycleState::Pending)
    }) {
        return Ok(false);
    }
    observations.retain(|row| crate::trigger_engine::durable::claim_conflict(&candidate, row));
    let doc_ids = observations
        .iter()
        .map(|row| row.document.clone())
        .collect::<Vec<_>>();
    let order = crate::trigger_engine::durable::request_arrival_order(txn, &doc_ids).await?;
    let positions = order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index as u64))
        .collect::<std::collections::HashMap<_, _>>();
    for row in &mut observations {
        row.arrival = positions.get(row.document.as_str()).copied();
    }
    let candidate = observations
        .iter()
        .find(|row| row.document == request.doc_id)
        .unwrap();
    Ok(crate::trigger_engine::durable::observed_claim_allowed(
        candidate,
        &observations,
    ))
}
