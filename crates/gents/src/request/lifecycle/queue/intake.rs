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
            crate::request::admission::SIGNED_REQUEST_FIELDS,
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
    order_pending_in_txn(txn, rows).await
}

pub(crate) async fn pending_with_new_candidate_in_txn(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    session_id: &str,
    candidate_doc_id: &str,
) -> Result<Vec<AgentRequestRow>> {
    let mut rows = pending_rows_in_txn(txn, node_did, session_id).await?;
    let index = rows
        .iter()
        .position(|row| row.doc_id.as_deref() == Some(candidate_doc_id))
        .context("new steering request is missing from its transaction")?;
    let candidate = rows.remove(index);
    anyhow::ensure!(
        !rows
            .iter()
            .any(|row| row.doc_id.as_deref() == Some(candidate_doc_id)),
        "new steering request is ambiguous"
    );
    let mut ordered = order_pending_in_txn(txn, rows).await?;
    ordered.push(candidate);
    Ok(ordered)
}

async fn order_pending_in_txn(
    txn: &ConfigApplyTxn<'_>,
    rows: Vec<AgentRequestRow>,
) -> Result<Vec<AgentRequestRow>> {
    let ids = rows
        .iter()
        .map(|row| {
            row.doc_id
                .clone()
                .context("pending message has no physical identity")
        })
        .collect::<Result<Vec<_>>>()?;
    let order = crate::triggers::engine::durable::request_arrival_order(txn, &ids).await?;
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

pub(crate) fn is_ordinary_queued(row: &AgentRequestRow) -> bool {
    row.execution_origin.as_deref() == Some("interactive")
        && row.caused_by_trigger_id.is_none()
        && row.caused_by_source_doc_id.is_none()
        && row.execution_generation.is_none()
        && row.admission_kind.as_deref() != Some("runtime-internal")
        && row.runtime_source_kind.is_none()
        && row.retry_parent_request.is_none()
        && row.retry_parent_request_doc_id.is_none()
        && row.input.as_ref().is_none_or(|input| {
            input.goal_continuation.is_none()
                && input.queue.as_ref().is_none_or(|queue| {
                    queue.delivery == QueueDelivery::Queue
                        && queue.policy == QueuePolicy::Append
                        && queue.key.is_none()
                        && queue.interrupted_request_id.is_none()
                        && queue.background_completion_wake_version.is_none()
                        && match queue.source {
                            QueueSource::User => true,
                            QueueSource::Steering => {
                                row.caused_by_parent_request_doc_id.is_some()
                                    && row.caused_by_parent_tool_call_doc_id.is_some()
                            }
                            _ => false,
                        }
                })
        })
}

pub(crate) fn boundary_candidate(rows: &[AgentRequestRow]) -> Option<&AgentRequestRow> {
    let mut passed_queue = false;
    for row in rows {
        if is_ordinary_queued(row) {
            passed_queue = true;
            continue;
        }
        let queue = row.input.as_ref().and_then(|input| input.queue.as_ref())?;
        if queue.delivery != QueueDelivery::Steer
            || !matches!(queue.source, QueueSource::User | QueueSource::Steering)
            || queue.policy != QueuePolicy::Append
            || queue.background_completion_wake_version.is_some()
        {
            return None;
        }
        let agent_steer = queue.source == QueueSource::Steering
            && row.admission_kind.as_deref() != Some("runtime-internal")
            && row.runtime_source_kind.is_none()
            && row.caused_by_parent_request_doc_id.is_some()
            && row.caused_by_parent_tool_call_doc_id.is_some();
        return (!passed_queue || agent_steer).then_some(row);
    }
    None
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum SteerAdmissionRefusal {
    #[error("the steer is incompatible with the active request scope; use delivery=queue")]
    Incompatible,
    #[error(
        "an earlier input prevents this steer from reaching the active turn; use delivery=queue"
    )]
    Blocked,
}

pub(crate) fn steer_reachable(
    head: &AgentRequest,
    mut pending: Vec<AgentRequestRow>,
    candidate_doc_id: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), SteerAdmissionRefusal> {
    while let Some(row) = boundary_candidate(&pending) {
        if !steering_compatible(head, row, now) {
            return Err(SteerAdmissionRefusal::Incompatible);
        }
        let id = row
            .doc_id
            .as_deref()
            .ok_or(SteerAdmissionRefusal::Blocked)?;
        if id == candidate_doc_id {
            return Ok(());
        }
        let id = id.to_owned();
        pending.retain(|row| row.doc_id.as_deref() != Some(id.as_str()));
    }
    Err(SteerAdmissionRefusal::Blocked)
}

pub(crate) async fn steering_snapshot(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
    verifier: &crate::request::admission::AgentRequestAdmissionVerifier,
) -> Result<gents_loop::loop_stream::SteeringSnapshot> {
    let rows = crate::store::config_client::ConfigAccess::transact_local_readonly(
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
    let mut remaining = rows;
    while let Some(row) = boundary_candidate(&remaining) {
        if !steering_compatible(head, row, chrono::Utc::now()) {
            break;
        }
        let id = row.doc_id.clone().unwrap_or_default();
        let request = AgentRequest::try_from(row.clone())?;
        match verifier.verify_fresh(&request, &head.agent_id).await {
            Ok(_) => {}
            Err(error) if !error.is_denied() => return Err(error.into()),
            Err(_) => break,
        }
        inputs.push(gents_loop::loop_stream::FoldedPrompt {
            key: folded_input_key(&id),
            message: request.input.authored_message(&request.content)?,
        });
        remaining.retain(|row| row.doc_id.as_deref() != Some(id.as_str()));
    }
    Ok(gents_loop::loop_stream::SteeringSnapshot {
        pending_request_doc_ids,
        inputs,
    })
}

pub(crate) async fn steering_inputs(
    node: &std::sync::Arc<EmbeddedNode>,
    head: &AgentRequest,
    verifier: &crate::request::admission::AgentRequestAdmissionVerifier,
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

pub(crate) fn steering_compatible(
    head: &AgentRequest,
    row: &AgentRequestRow,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    matches!(
        crate::request::lifecycle::parse_valid_until(row.valid_until.as_deref(), now),
        crate::request::lifecycle::TtlOutcome::NotSet
            | crate::request::lifecycle::TtlOutcome::Live(_)
    ) && row.input.as_ref().is_some_and(|input| {
        input.queue.as_ref().is_some_and(|queue| {
            queue.delivery == QueueDelivery::Steer
                && match queue.source {
                    QueueSource::User => true,
                    // Lean `QueuedSteering.agentSteerTargets`.
                    QueueSource::Steering => {
                        queue.queued_after_request_id.as_deref() == Some(head.request_id.as_str())
                    }
                    _ => false,
                }
                && queue.policy == QueuePolicy::Append
                && queue.key.is_none()
                && queue.interrupted_request_id.is_none()
                && queue.background_completion_wake_version.is_none()
        }) && input.cwd == head.input.cwd
            && input.selected_skill_ids == head.input.selected_skill_ids
            && input.initial_title.is_none()
            && input.goal_continuation.is_none()
            && super::folding::same_goal_assignment(head, row)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_selection_matches_lean() {
        let cases = &crate::lean_vocab_test::lean_contract_snapshot().agent_steering_boundary_cases;
        for case in cases {
            let rows = case
                .inputs
                .iter()
                .map(|input| {
                    let (delivery, source, parent) = match input.kind.as_str() {
                        "queued" => (QueueDelivery::Queue, QueueSource::User, None),
                        "user_steer" => (QueueDelivery::Steer, QueueSource::User, None),
                        "blocked" => (
                            QueueDelivery::Queue,
                            QueueSource::BackgroundCompletion,
                            None,
                        ),
                        "agent_steer" => (
                            QueueDelivery::Steer,
                            QueueSource::Steering,
                            Some("tool".to_owned()),
                        ),
                        other => panic!("unknown boundary kind {other}"),
                    };
                    AgentRequestRow {
                        doc_id: Some(input.id.to_string()),
                        request_id: input.id.to_string(),
                        execution_origin: Some("interactive".to_owned()),
                        caused_by_parent_request_doc_id: parent
                            .as_ref()
                            .map(|_| "request".to_owned()),
                        caused_by_parent_tool_call_doc_id: parent,
                        input: Some(RequestInput {
                            queue: Some(RequestQueue {
                                delivery,
                                source,
                                policy: QueuePolicy::Append,
                                position: None,
                                key: None,
                                queued_after_request_id: None,
                                interrupted_request_id: None,
                                background_completion_wake_version: None,
                            }),
                            ..RequestInput::default()
                        }),
                        ..AgentRequestRow::default()
                    }
                })
                .collect::<Vec<_>>();
            let actual =
                boundary_candidate(&rows).map(|row| row.request_id.parse::<u64>().unwrap());
            assert_eq!(actual, case.expected, "{case:?}");
            if let Some(index) = case.inputs.iter().position(|input| input.kind == "blocked") {
                let mut wake = rows.clone();
                let queue = wake[index].input.as_mut().unwrap().queue.as_mut().unwrap();
                queue.source = QueueSource::User;
                queue.background_completion_wake_version = Some(1);
                assert_eq!(
                    boundary_candidate(&wake).map(|row| row.request_id.parse::<u64>().unwrap()),
                    case.expected,
                    "wake marker {case:?}"
                );
                let mut internal = rows.clone();
                let queue = internal[index]
                    .input
                    .as_mut()
                    .unwrap()
                    .queue
                    .as_mut()
                    .unwrap();
                queue.source = QueueSource::Goal;
                assert_eq!(
                    boundary_candidate(&internal).map(|row| row.request_id.parse::<u64>().unwrap()),
                    case.expected,
                    "internal source {case:?}"
                );
                let mut internal_issuance = rows.clone();
                let queue = internal_issuance[index]
                    .input
                    .as_mut()
                    .unwrap()
                    .queue
                    .as_mut()
                    .unwrap();
                queue.source = QueueSource::User;
                internal_issuance[index].admission_kind = Some("runtime-internal".to_owned());
                assert_eq!(
                    boundary_candidate(&internal_issuance)
                        .map(|row| row.request_id.parse::<u64>().unwrap()),
                    case.expected,
                    "internal issuance {case:?}"
                );
            }
        }
    }
}
