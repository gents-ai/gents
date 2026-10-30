use super::*;
use crate::graphql::{cursor_after_arg, cursor_page};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;

const CLAIM_QUEUE_PAGE_SIZE: usize = 64;

#[derive(serde::Deserialize)]
struct QueueReceipt {
    request_id: String,
    trigger_id: String,
    queued_serial: bool,
}

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

/// The read and winning request CAS must share the transaction's mutation gate.
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
    let candidate_doc_id = escape_graphql_string(&request.doc_id);
    let response = txn
        .execute_local_response(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{candidate_doc_id}" }},
            node_did: {{ _eq: "{owner}" }}, purpose: {{ _eq: "normal" }},
            lifecycle_state: {{ _in: {nonterminal} }} }}, limit: 1) {{
            _docID request_id session_id lifecycle_state
        }} }}"#,
        ))
        .await?;
    let Some(candidate_row) =
        crate::graphql::first_row::<AgentRequestRow>(&response, "AgentRequest")?
    else {
        return Ok(false);
    };
    if candidate_row.lifecycle_state != Some(RequestLifecycleState::Pending) {
        return Ok(false);
    }
    let candidate_request_id = candidate_row.request_id.clone();
    let session_filter = match candidate_row.session_id.as_deref() {
        Some(session_id) if !session_id.is_empty() => format!(
            "session_id: {{ _eq: \"{}\" }}",
            escape_graphql_string(session_id)
        ),
        Some(_) | None => {
            "_or: [{ session_id: { _eq: null } }, { session_id: { _eq: \"\" } }]".to_owned()
        }
    };
    let mut session_requests = Vec::new();
    let mut after = None;
    loop {
        let after_arg = cursor_after_arg(after.as_deref());
        let response = txn
            .execute_local_response(&format!(
                r#"{{ _cursor {{ AgentRequest(first: {CLAIM_QUEUE_PAGE_SIZE}{after_arg},
                filter: {{ node_did: {{ _eq: "{owner}" }}, purpose: {{ _eq: "normal" }},
                lifecycle_state: {{ _in: {nonterminal} }}, {session_filter} }},
                order: [{{ session_id: ASC }}]) {{ _docID request_id session_id lifecycle_state }}
                _pageInfo {{ hasNext endCursor }} }} }}"#,
            ))
            .await?;
        let (mut page, next) = cursor_page::<AgentRequestRow>(&response, "AgentRequest")?;
        session_requests.append(&mut page);
        let Some(next) = next else { break };
        anyhow::ensure!(
            after.as_deref() != Some(next.as_str()),
            "claim session cursor did not advance"
        );
        after = Some(next);
    }

    let session_request_ids = session_requests
        .iter()
        .map(|row| row.request_id.as_str())
        .collect::<Vec<_>>();
    let session_receipts =
        claim_receipts_for_request_ids(txn, &request.node_did, &session_request_ids).await?;

    let candidate_receipt = session_receipts.get(candidate_request_id.as_str());
    let serial_trigger = candidate_receipt
        .filter(|receipt| receipt.queued_serial)
        .map(|receipt| receipt.trigger_id.clone());

    let mut observations = Vec::with_capacity(session_requests.len());
    let mut observation_by_doc = rapidhash::RapidHashMap::with_capacity_and_hasher(
        session_requests.len(),
        Default::default(),
    );
    for row in &session_requests {
        let observation = claim_observation(
            row,
            &request.node_did,
            session_receipts.get(row.request_id.as_str()),
        );
        observation_by_doc.insert(observation.document.clone(), observations.len());
        observations.push(observation);
    }

    if serial_trigger.is_some() {
        let mut after_request = None;
        loop {
            let after_arg = cursor_after_arg(after_request.as_deref());
            let response = txn
                .execute_local_response(&format!(
                    r#"{{ _cursor {{ AgentRequest(first: {CLAIM_QUEUE_PAGE_SIZE}{after_arg},
                    filter: {{ node_did: {{ _eq: "{owner}" }}, purpose: {{ _eq: "normal" }},
                    lifecycle_state: {{ _in: {nonterminal} }} }},
                    order: [{{ node_did: ASC }}]) {{ _docID request_id session_id lifecycle_state }}
                    _pageInfo {{ hasNext endCursor }} }} }}"#,
                ))
                .await?;
            let (request_page, next_request) =
                cursor_page::<AgentRequestRow>(&response, "AgentRequest")?;
            let mut receipt_request_ids = request_page
                .iter()
                .map(|row| row.request_id.as_str())
                .collect::<Vec<_>>();
            receipt_request_ids.sort_unstable();
            receipt_request_ids.dedup();
            let receipt_by_request =
                claim_receipts_for_request_ids(txn, &request.node_did, &receipt_request_ids)
                    .await?;
            for row in request_page {
                let Some(receipt) = receipt_by_request.get(row.request_id.as_str()) else {
                    continue;
                };
                let document = row.doc_id.clone().unwrap_or_default();
                if let Some(index) = observation_by_doc.get(document.as_str()).copied() {
                    observations[index].trigger = receipt.trigger_id.clone();
                    observations[index].serial = receipt.queued_serial;
                    observations[index].receipt = true;
                } else {
                    let observation = claim_observation(&row, &request.node_did, Some(receipt));
                    observation_by_doc.insert(observation.document.clone(), observations.len());
                    observations.push(observation);
                }
            }

            let Some(next_request) = next_request else {
                break;
            };
            anyhow::ensure!(
                after_request.as_deref() != Some(next_request.as_str()),
                "claim request cursor did not advance"
            );
            after_request = Some(next_request);
        }
    }

    let Some(candidate) = observations
        .iter()
        .find(|row| row.document == request.doc_id)
        .cloned()
    else {
        return Ok(false);
    };
    observations.retain(|row| crate::triggers::engine::durable::claim_conflict(&candidate, row));
    let doc_ids = observations
        .iter()
        .map(|row| row.document.clone())
        .collect::<Vec<_>>();
    let order = crate::triggers::engine::durable::request_arrival_order(txn, &doc_ids).await?;
    let positions = order
        .iter()
        .enumerate()
        .map(|(index, id)| (id.as_str(), index as u64))
        .collect::<rapidhash::RapidHashMap<_, _>>();
    for row in &mut observations {
        row.arrival = positions.get(row.document.as_str()).copied();
    }
    let candidate = observations
        .iter()
        .find(|row| row.document == request.doc_id)
        .unwrap();
    Ok(crate::triggers::engine::durable::observed_claim_allowed(
        candidate,
        &observations,
    ))
}

async fn claim_receipts_for_request_ids(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    owner_did: &str,
    request_ids: &[&str],
) -> Result<rapidhash::RapidHashMap<String, QueueReceipt>> {
    let owner = escape_graphql_string(owner_did);
    let mut receipts = rapidhash::RapidHashMap::default();
    for request_batch in request_ids.chunks(CLAIM_QUEUE_PAGE_SIZE) {
        let mut seen_ids = rapidhash::RapidHashSet::default();
        let ids = crate::graphql::graphql_string_list_literal(
            request_batch
                .iter()
                .filter_map(|request_id| seen_ids.insert(*request_id).then_some(*request_id)),
        );
        let mut after = None;
        loop {
            let after_arg = cursor_after_arg(after.as_deref());
            let response = txn
                .execute_local_response(&format!(
                    r#"{{ _cursor {{ TriggerFire(first: {CLAIM_QUEUE_PAGE_SIZE}{after_arg},
                    filter: {{ owner_did: {{ _eq: "{owner}" }}, request_id: {{ _in: {ids} }} }},
                    order: [{{ request_id: ASC }}]) {{ request_id trigger_id queued_serial }}
                    _pageInfo {{ hasNext endCursor }} }} }}"#,
                ))
                .await?;
            let (page, next) = cursor_page::<QueueReceipt>(&response, "TriggerFire")?;
            for receipt in page {
                receipts.insert(receipt.request_id.clone(), receipt);
            }
            let Some(next) = next else { break };
            anyhow::ensure!(
                after.as_deref() != Some(next.as_str()),
                "claim receipt cursor did not advance"
            );
            after = Some(next);
        }
    }
    Ok(receipts)
}

fn claim_observation(
    row: &AgentRequestRow,
    owner: &str,
    receipt: Option<&QueueReceipt>,
) -> crate::triggers::engine::durable::ClaimObservation {
    crate::triggers::engine::durable::ClaimObservation {
        document: row.doc_id.clone().unwrap_or_default(),
        owner: owner.to_owned(),
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
}
