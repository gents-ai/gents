use super::*;
use crate::config_client::ConfigRead;

#[derive(Debug, thiserror::Error)]
#[error("replacement request does not own its queue position")]
pub(crate) struct InvalidQueuePosition;

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
    let mut visited = std::collections::HashSet::new();
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
                crate::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let previous: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())?;
        let [previous] = previous.as_slice() else {
            return Err(InvalidQueuePosition.into());
        };
        crate::request_admission::verify_request_receipt_signature(previous)
            .map_err(|_| InvalidQueuePosition)?;
        let old_input = previous.input.as_ref().ok_or(InvalidQueuePosition)?;
        let old_queue = old_input.queue.as_ref().ok_or(InvalidQueuePosition)?;
        anyhow::ensure!(
            previous.doc_id.as_deref() == Some(position.replaces_request_doc_id.as_str())
                && previous.lifecycle_state == Some(RequestLifecycleState::Superseded)
                && previous.superseded_by_request_doc_id == current.doc_id
                && previous.failure_reason.as_deref() == Some("pending message replaced")
                && previous.admission_signer_did == previous.requester_did
                && previous.node_did == current.node_did
                && previous.requester_did == current.requester_did
                && previous.session_id == current.session_id
                && previous.agent_id == current.agent_id
                && previous.workspace_id == current.workspace_id
                && previous.workspace_owner_node_did == current.workspace_owner_node_did
                && previous.workspace_authority == current.workspace_authority
                && previous.workspace_seal_hash == current.workspace_seal_hash
                && old_input.cwd == input.cwd
                && old_input.selected_skill_ids == input.selected_skill_ids
                && old_queue.source == QueueSource::User
                && queue.source == QueueSource::User
                && old_queue.policy == QueuePolicy::Append
                && queue.policy == QueuePolicy::Append
                && old_queue.delivery == queue.delivery,
            InvalidQueuePosition
        );
        current = previous.clone();
    }
}

/// A malformed position cannot affect scheduling before admission rejects it.
/// Valid replacements retain the original native arrival slot across edits.
pub(crate) async fn effective_slots(
    txn: &ConfigApplyTxn<'_>,
    ids: &[String],
) -> Result<Vec<(String, String)>> {
    let literal = crate::graphql::graphql_string_list_literal(ids.iter().map(String::as_str));
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ _docID: {{ _in: {literal} }} }}) {{
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
    use crate::identity::{KeyIdentity, NodeIdentity};
    use gents_protocol::request_admission::{
        AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose,
    };
    use gents_protocol::request_input::{QueueDelivery, QueuePosition};

    struct Rows(Vec<AgentRequestRow>);
    #[async_trait::async_trait]
    impl ConfigRead for Rows {
        async fn execute_read(&self, query: &str) -> Result<serde_json::Value> {
            let rows = self
                .0
                .iter()
                .filter(|row| {
                    query.contains(&format!("_eq: \"{}\"", row.doc_id.as_deref().unwrap()))
                })
                .map(|row| {
                    let mut value = serde_json::to_value(row).unwrap();
                    value["_docID"] = serde_json::json!(row.doc_id);
                    value
                })
                .collect::<Vec<_>>();
            Ok(serde_json::json!({"data":{"AgentRequest":rows}}))
        }
    }

    async fn signed_row(
        identity: &KeyIdentity,
        id: &str,
        position: Option<QueuePosition>,
    ) -> AgentRequestRow {
        let mut create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            id,
            identity.did(),
            identity.did(),
            "agent",
            "session",
            "text",
            "interactive",
            "2026-10-10T00:00:00Z",
            AgentRequestAdmissionRecord::local_self(identity.did()),
        );
        create.input.queue = Some(RequestQueue {
            source: QueueSource::User,
            policy: QueuePolicy::Append,
            delivery: QueueDelivery::Steer,
            position,
            key: None,
            queued_after_request_id: None,
            interrupted_request_id: None,
            background_completion_wake_version: None,
        });
        crate::sign_agent_request_create(identity, &mut create)
            .await
            .unwrap();
        AgentRequestRow {
            doc_id: Some(id.into()),
            request_id: create.request_id,
            purpose: Some(create.purpose),
            node_did: Some(create.node_did),
            requester_did: Some(create.requester_did),
            agent_id: Some(create.agent_id),
            session_id: Some(create.session_id),
            content: Some(create.content),
            input: Some(create.input),
            execution_origin: Some(create.execution_origin),
            created_at: Some(create.created_at),
            retry_root_request: create.retry_root_request,
            retry_count: Some(create.retry_count),
            max_retries: Some(create.max_retries),
            request_hop: Some(i64::from(create.request_hop)),
            admission_kind: Some(create.admission.kind.as_str().into()),
            admission_signer_did: Some(create.admission.signer_did),
            admission_signature: Some(bs58::encode(&create.admission.signature).into_string()),
            lifecycle_state: Some(RequestLifecycleState::Pending),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn inherited_slot_requires_authenticated_chain_to_original_physical_document() {
        let temp = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(temp.path().join("position.key"), None).unwrap();
        let mut original = signed_row(&identity, "original", None).await;
        let mut replacement = signed_row(
            &identity,
            "replacement",
            Some(QueuePosition {
                slot_request_doc_id: "original".into(),
                replaces_request_doc_id: "original".into(),
            }),
        )
        .await;
        let final_row = signed_row(
            &identity,
            "final",
            Some(QueuePosition {
                slot_request_doc_id: "original".into(),
                replaces_request_doc_id: "replacement".into(),
            }),
        )
        .await;
        original.lifecycle_state = Some(RequestLifecycleState::Superseded);
        original.superseded_by_request_doc_id = Some("replacement".into());
        original.failure_reason = Some("pending message replaced".into());
        replacement.lifecycle_state = Some(RequestLifecycleState::Superseded);
        replacement.superseded_by_request_doc_id = Some("final".into());
        replacement.failure_reason = Some("pending message replaced".into());
        validate_position(
            &Rows(vec![original.clone(), replacement.clone()]),
            &final_row,
        )
        .await
        .unwrap();
        original.content = Some("changed after signing".into());
        assert!(
            validate_position(&Rows(vec![original, replacement]), &final_row)
                .await
                .unwrap_err()
                .is::<InvalidQueuePosition>()
        );
    }
}
