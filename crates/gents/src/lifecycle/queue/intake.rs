use super::*;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_input::QueueDelivery;

pub(crate) async fn pending_rows_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<AgentRequestRow>> {
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }},
            lifecycle_state: {{ _eq: "pending" }} }}) {{
            {} execution_lease_secs interrupt_requested_at
        }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(session_id),
            crate::request_admission::SIGNED_REQUEST_FIELDS,
        ))
        .await?;
    serde_json::from_value(response["data"]["AgentRequest"].clone())
        .context("decode pending session requests")
}

pub(crate) async fn pending_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<AgentRequestRow>> {
    let rows = pending_rows_in_txn(txn, node_did, session_id).await?;
    let ids = rows
        .iter()
        .map(|row| {
            row.doc_id
                .clone()
                .context("pending message has no physical identity")
        })
        .collect::<Result<Vec<_>>>()?;
    let order = crate::trigger_engine::durable::request_arrival_order(txn, &ids).await?;
    anyhow::ensure!(
        order.len() == ids.len(),
        "pending queue arrival is incomplete"
    );
    let mut by_id = rows
        .into_iter()
        .map(|row| (row.doc_id.clone().unwrap(), row))
        .collect::<rapidhash::RapidHashMap<_, _>>();
    order
        .into_iter()
        .map(|id| by_id.remove(&id).context("pending queue arrival changed"))
        .collect()
}

pub(crate) async fn steering_snapshot(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
    verifier: &crate::request_admission::AgentRequestAdmissionVerifier,
) -> Result<gents_loop::loop_stream::SteeringSnapshot> {
    let rows = crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "queue.steering_snapshot",
        |txn| Box::pin(async move { pending_in_txn(txn, &head.node_did, &head.session_id).await }),
    )
    .await?;
    let mut pending_request_doc_ids = rows
        .iter()
        .filter_map(|row| row.doc_id.clone())
        .collect::<Vec<_>>();
    pending_request_doc_ids.sort();
    let mut inputs = Vec::new();
    for row in rows {
        if !steering_compatible(head, &row, chrono::Utc::now()) {
            break;
        }
        let id = row.doc_id.clone().unwrap_or_default();
        let request = AgentRequest::try_from(row)?;
        match verifier.verify_fresh(&request, &head.agent_id).await {
            Ok(_) => {}
            Err(error) if !error.is_denied() => return Err(error.into()),
            Err(_) => break,
        }
        inputs.push(gents_loop::loop_stream::FoldedPrompt {
            key: folded_input_key(&id),
            message: request.input.authored_message(&request.content)?,
        });
    }
    Ok(gents_loop::loop_stream::SteeringSnapshot {
        pending_request_doc_ids,
        inputs,
    })
}

pub(crate) async fn steering_inputs(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
    verifier: &crate::request_admission::AgentRequestAdmissionVerifier,
) -> Result<Vec<gents_loop::loop_stream::FoldedPrompt>> {
    Ok(steering_snapshot(node, head, verifier).await?.inputs)
}

pub(crate) async fn pending_ids_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<String>> {
    let mut ids = pending_rows_in_txn(txn, node_did, session_id)
        .await?
        .into_iter()
        .map(|row| row.doc_id.context("pending input lacks physical identity"))
        .collect::<Result<Vec<_>>>()?;
    ids.sort();
    Ok(ids)
}

pub(super) fn steering_requester_allowed(head: &AgentRequest, row: &AgentRequestRow) -> bool {
    if agent_steer(row) {
        [
            row.caused_by_parent_request_id.as_deref(),
            row.caused_by_parent_request_doc_id.as_deref(),
            row.caused_by_parent_tool_call_id.as_deref(),
            row.caused_by_parent_tool_call_doc_id.as_deref(),
        ]
        .iter()
        .all(|id| id.is_some_and(|id| !id.trim().is_empty()))
    } else {
        row.requester_did == head.requester_did
    }
}

fn agent_steer(row: &AgentRequestRow) -> bool {
    let Some(input) = row.input.as_ref() else {
        return false;
    };
    input
        .queue
        .as_ref()
        .is_some_and(|queue| queue.source == QueueSource::Steering)
        && (input.steer_target_request_doc_id.is_some()
            || row
                .caused_by_parent_tool_call_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty())
            || row
                .caused_by_parent_tool_call_doc_id
                .as_deref()
                .is_some_and(|id| !id.trim().is_empty()))
}

pub(super) fn steering_context_allowed(head: &AgentRequest, row: &AgentRequestRow) -> bool {
    let Some(input) = row.input.as_ref() else {
        return false;
    };
    if agent_steer(row) {
        input.cwd.is_none()
            && input.selected_skill_ids.is_empty()
            && row.workspace_id.is_none()
            && row.workspace_owner_node_did.is_none()
            && row.workspace_authority.is_none()
            && row.workspace_seal_hash.is_none()
    } else {
        input.cwd == head.input.cwd
            && input.selected_skill_ids == head.input.selected_skill_ids
            && row.workspace_id == head.workspace_id
            && row.workspace_owner_node_did == head.workspace_owner_node_did
            && row.workspace_authority == head.workspace_authority
            && row.workspace_seal_hash == head.workspace_seal_hash
    }
}

pub(super) fn steering_compatible(
    head: &AgentRequest,
    row: &AgentRequestRow,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    matches!(
        crate::lifecycle::parse_valid_until(row.valid_until.as_deref(), now),
        crate::lifecycle::TtlOutcome::NotSet | crate::lifecycle::TtlOutcome::Live(_)
    ) && row.input.as_ref().is_some_and(|input| {
        input.queue.as_ref().is_some_and(|queue| {
            queue.delivery == QueueDelivery::Steer
                && matches!(queue.source, QueueSource::User | QueueSource::Steering)
                && queue.policy == QueuePolicy::Append
                && queue.key.is_none()
                && queue.interrupted_request_id.is_none()
                && queue.background_completion_wake_version.is_none()
        }) && steering_context_allowed(head, row)
            && if agent_steer(row) {
                input.steer_target_request_doc_id.as_deref() == Some(head.doc_id.as_str())
            } else {
                input.steer_target_request_doc_id.is_none()
            }
            && input.initial_title.is_none()
            && input.goal_continuation.is_none()
            && super::folding::same_goal_assignment(head, row)
    }) && row.purpose == Some(RequestPurpose::Normal)
        && row.execution_origin.as_deref() == Some("interactive")
        && row.node_did.as_deref() == Some(head.node_did.as_str())
        && row.session_id.as_deref() == Some(head.session_id.as_str())
        && steering_requester_allowed(head, row)
        && row.agent_id.as_deref() == Some(head.agent_id.as_str())
        && row.execution_generation.is_none()
        && row.interrupt_requested_at.is_none()
        && row.retry_parent_request.is_none()
        && row.caused_by_trigger_id.is_none()
        && row.caused_by_source_doc_id.is_none()
}
