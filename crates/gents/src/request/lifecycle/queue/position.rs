use super::*;
use crate::config_client::ConfigRead;

#[derive(Debug, thiserror::Error)]
#[error("replacement request does not own its queue position")]
pub(crate) struct InvalidQueuePosition;

pub(super) fn same_execution_directory(old: &RequestInput, new: &RequestInput) -> bool {
    old.cwd == new.cwd
}

pub async fn validate_position(read: &impl ConfigRead, row: &AgentRequestRow) -> Result<()> {
    let Some(position) = row
        .input
        .as_ref()
        .and_then(|input| input.queue.as_ref())
        .and_then(|queue| queue.position.as_ref())
    else {
        return Ok(());
    };
    let expected_slot = position.slot_request_doc_id.clone();
    anyhow::ensure!(!expected_slot.is_empty(), InvalidQueuePosition);
    let mut current = row.clone();
    let mut visited = rapidhash::RapidHashSet::default();
    loop {
        let current_id = current.doc_id.as_deref().ok_or(InvalidQueuePosition)?;
        anyhow::ensure!(visited.insert(current_id.to_owned()), InvalidQueuePosition);
        let input = current.input.as_ref().ok_or(InvalidQueuePosition)?;
        let queue = input.queue.as_ref().ok_or(InvalidQueuePosition)?;
        let Some(position) = queue.position.as_ref() else {
            anyhow::ensure!(current_id == expected_slot, InvalidQueuePosition);
            return Ok(());
        };
        anyhow::ensure!(
            position.slot_request_doc_id == expected_slot
                && !position.replaces_request_doc_id.is_empty()
                && !visited.contains(&position.replaces_request_doc_id),
            InvalidQueuePosition
        );
        let response = read
            .execute_read(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                {} superseded_by_request_doc_id failure_reason
            }} }}"#,
                escape_graphql_string(&position.replaces_request_doc_id),
                crate::request::admission::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let previous: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())?;
        let [previous] = previous.as_slice() else {
            return Err(InvalidQueuePosition.into());
        };
        crate::request::admission::verify_request_receipt_signature(previous)
            .map_err(|_| InvalidQueuePosition)?;
        let old_input = previous.input.as_ref().ok_or(InvalidQueuePosition)?;
        let old_queue = old_input.queue.as_ref().ok_or(InvalidQueuePosition)?;
        anyhow::ensure!(
            previous.doc_id.as_deref() == Some(position.replaces_request_doc_id.as_str())
                && previous.lifecycle_state == Some(RequestLifecycleState::Superseded)
                && previous.superseded_by_request_doc_id == current.doc_id
                && previous.failure_reason.as_deref() == Some(super::management::REPLACED_REASON)
                && previous.admission_signer_did == previous.requester_did
                && previous.node_did == current.node_did
                && previous.requester_did == current.requester_did
                && previous.session_id == current.session_id
                && previous.agent_id == current.agent_id
                && previous.workspace_id == current.workspace_id
                && previous.workspace_owner_node_did == current.workspace_owner_node_did
                && previous.workspace_authority == current.workspace_authority
                && previous.workspace_seal_hash == current.workspace_seal_hash
                && same_execution_directory(old_input, input)
                && old_queue.source == QueueSource::User
                && queue.source == QueueSource::User
                && old_queue.policy == QueuePolicy::Append
                && queue.policy == QueuePolicy::Append
                && super::management::replacement_delivery_allowed(
                    old_queue.delivery,
                    queue.delivery
                ),
            InvalidQueuePosition
        );
        current = previous.clone();
    }
}

pub(crate) async fn effective_slots(
    txn: &ConfigApplyTxn<'_>,
    ids: &[String],
) -> Result<Vec<(String, String)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let literal = crate::graphql::graphql_string_list_literal(ids.iter().map(String::as_str));
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: {literal}) {{
            _docID request_id node_did requester_did session_id agent_id input
            workspace_id workspace_owner_node_did workspace_authority workspace_seal_hash
        }} }}"#,
        ))
        .await?;
    let rows: Vec<AgentRequestRow> =
        serde_json::from_value(response["data"]["AgentRequest"].clone())?;
    let mut result = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row
            .doc_id
            .clone()
            .context("queue row missing physical identity")?;
        let slot = match validate_position(txn, &row).await {
            Ok(()) => row
                .input
                .as_ref()
                .and_then(|input| input.queue.as_ref())
                .and_then(|queue| queue.position.as_ref())
                .map(|position| position.slot_request_doc_id.clone())
                .unwrap_or_else(|| id.clone()),
            Err(error) if error.is::<InvalidQueuePosition>() => id.clone(),
            Err(error) => return Err(error),
        };
        result.push((id, slot));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn effective_slot_batch_uses_only_found_physical_requests() {
        use crate::config_client::ConfigAccess;
        use crate::defra_node::EmbeddedNode;

        let dir = tempfile::tempdir().unwrap();
        let node = std::sync::Arc::new(
            crate::test_support::node_builder()
                .data_path(dir.path().join("node"))
                .build()
                .await
                .unwrap(),
        );
        crate::ensure_runtime_schemas(&node).await.unwrap();

        async fn create_request(
            node: &std::sync::Arc<EmbeddedNode>,
            request_id: &str,
            node_did: &str,
            session_id: &str,
        ) -> String {
            let mutation = format!(
                r#"mutation {{ create_AgentRequest(input: {{
                    purpose: "normal", request_id: "{request_id}",
                    node_did: "{node_did}", requester_did: "did:test:requester",
                    agent_id: "worker", session_id: "{session_id}", content: "work",
                    lifecycle_state: "completed", backend_id: "",
                    execution_origin: "interactive", failure_reason: "",
                    created_at: "2026-10-10T00:00:00Z", retry_count: 0, max_retries: 3
                }}) {{ _docID }} }}"#
            );
            let created =
                ConfigAccess::write_local_response(node, "test.queue_position", &mutation)
                    .await
                    .unwrap();
            let data = created.data.unwrap();
            data.as_object().unwrap().values().next().unwrap()[0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned()
        }

        let requested = create_request(&node, "requested", "did:test:node", "queue").await;
        let _foreign = create_request(&node, "foreign", "did:test:other", "other-queue").await;
        let missing =
            document::DocID::new_v0_from_seed("missing-queue-position-request").to_string();
        let ids = vec![requested.clone(), missing];
        let slots = ConfigAccess::transact_local_readonly(
            &node,
            None,
            "test.queue_position_direct_ids",
            |txn| {
                let ids = ids.clone();
                Box::pin(async move { effective_slots(txn, &ids).await })
            },
        )
        .await
        .unwrap();
        assert_eq!(slots, vec![(requested.clone(), requested)]);
        node.shutdown().await;
    }
}
