use super::*;
use gents_protocol::request_admission::RequestPurpose;
use gents_protocol::request_input::QueueDelivery;

pub(crate) async fn steering_snapshot(
    node: &EmbeddedNode,
    head: &AgentRequest,
    verifier: &crate::request_admission::AgentRequestAdmissionVerifier,
) -> Result<gents_loop::loop_stream::SteeringSnapshot> {
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }},
            session_id: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }},
            lifecycle_state: {{ _eq: "pending" }} }}) {{
            {} lifecycle_state interrupt_requested_at valid_until retry_parent_request
        }} }}"#,
            escape_graphql_string(&head.node_did),
            escape_graphql_string(&head.session_id),
            crate::watcher::AGENT_REQUEST_FIELDS,
        ),
        "pending steering input",
    )
    .await?;
    let rows: Vec<AgentRequestRow> = crate::graphql::rows(&response, "AgentRequest")?;
    let ids = rows
        .iter()
        .map(|row| {
            row.doc_id
                .clone()
                .context("pending input lacks physical identity")
        })
        .collect::<Result<Vec<_>>>()?;
    let order = crate::config_client::ConfigAccess::transact_local_readonly(
        node,
        None,
        "queue.steering_order",
        |txn| {
            let ids = &ids;
            Box::pin(async move {
                crate::trigger_engine::durable::request_arrival_order(txn, ids).await
            })
        },
    )
    .await?;
    anyhow::ensure!(
        order.len() == ids.len(),
        "pending steering queue order is incomplete"
    );
    let mut rows = rows
        .into_iter()
        .map(|row| (row.doc_id.clone().unwrap(), row))
        .collect::<std::collections::HashMap<_, _>>();
    let mut pending_request_doc_ids = ids.clone();
    pending_request_doc_ids.sort();
    let mut inputs = Vec::new();
    for id in order {
        let row = rows.remove(&id).context("pending input changed identity")?;
        let compatible = steering_compatible(head, &row, chrono::Utc::now());
        if !compatible {
            break;
        }
        let request = AgentRequest::try_from(row)?;
        match verifier.verify_fresh(&request, &head.agent_id).await {
            Ok(_) => {}
            Err(error) if !error.is_denied() => return Err(error.into()),
            Err(_) => break,
        }
        inputs.push(gents_loop::loop_stream::FoldedPrompt {
            key: folded_input_key(&id),
            message: gents_protocol::message::Message::user(request.content),
        });
    }
    Ok(gents_loop::loop_stream::SteeringSnapshot {
        pending_request_doc_ids,
        inputs,
    })
}

pub(crate) async fn steering_inputs(
    node: &EmbeddedNode,
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
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }},
        session_id: {{ _eq: "{}" }}, purpose: {{ _eq: "normal" }},
        lifecycle_state: {{ _eq: "pending" }} }}) {{ _docID }} }}"#,
            escape_graphql_string(node_did),
            escape_graphql_string(session_id)
        ))
        .await?;
    #[derive(serde::Deserialize)]
    struct PendingIdentity {
        #[serde(rename = "_docID")]
        doc_id: Option<String>,
    }
    let rows: Vec<PendingIdentity> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())
            .context("decode pending input identities")?;
    let mut ids = rows
        .into_iter()
        .map(|row| row.doc_id.context("pending input lacks physical identity"))
        .collect::<Result<Vec<_>>>()?;
    ids.sort();
    Ok(ids)
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
        }) && input.cwd == head.input.cwd
            && input.selected_skill_ids == head.input.selected_skill_ids
            && input.initial_title.is_none()
            && input.goal_continuation.is_none()
    }) && row.purpose == Some(RequestPurpose::Normal)
        && row.execution_origin.as_deref() == Some("interactive")
        && row.node_did.as_deref() == Some(head.node_did.as_str())
        && row.session_id.as_deref() == Some(head.session_id.as_str())
        && row.requester_did == head.requester_did
        && row.agent_id.as_deref() == Some(head.agent_id.as_str())
        && row.workspace_id == head.workspace_id
        && row.workspace_owner_node_did == head.workspace_owner_node_did
        && row.workspace_authority == head.workspace_authority
        && row.workspace_seal_hash == head.workspace_seal_hash
        && row.execution_generation.is_none()
        && row.interrupt_requested_at.is_none()
        && row.retry_parent_request.is_none()
        && row.caused_by_trigger_id.is_none()
        && row.caused_by_source_doc_id.is_none()
}
