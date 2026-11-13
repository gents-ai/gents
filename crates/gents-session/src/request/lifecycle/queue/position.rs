use super::*;
use gents_store::config_client::ConfigRead;

#[derive(Debug, thiserror::Error)]
#[error("replacement request does not own its queue position")]
pub struct InvalidQueuePosition;

#[derive(Debug, thiserror::Error)]
pub enum PositionError {
    #[error(transparent)]
    Invalid(#[from] InvalidQueuePosition),
    #[error(transparent)]
    Access(#[from] gents_store::config_client::ConfigClientError),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl PositionError {
    /// The refusal, or the failure erased as its own root: a transparent
    /// variant hides the wrapped error from `source()`, so wrapping this enum
    /// would hide an access error from classifiers of the erased error.
    pub(crate) fn refusal(self) -> std::result::Result<InvalidQueuePosition, anyhow::Error> {
        match self {
            Self::Invalid(invalid) => Ok(invalid),
            Self::Access(error) => Err(error.into()),
            Self::Other(error) => Err(error),
        }
    }
}

impl gents_store::config_client::TransactionFailure for PositionError {
    fn config_client_error(&self) -> Option<&gents_store::config_client::ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Other(error) => {
                gents_store::config_client::TransactionFailure::config_client_error(error)
            }
            Self::Invalid(_) => None,
        }
    }

    fn rerun_requested(&self) -> Option<&gents_store::config_client::RerunNote> {
        match self {
            Self::Access(error) => {
                gents_store::config_client::TransactionFailure::rerun_requested(error)
            }
            Self::Other(error) => {
                gents_store::config_client::TransactionFailure::rerun_requested(error)
            }
            Self::Invalid(_) => None,
        }
    }
}

pub(super) fn same_execution_directory(old: &RequestInput, new: &RequestInput) -> bool {
    old.cwd == new.cwd
}

pub async fn validate_position(
    read: &impl ConfigRead,
    row: &AgentRequestRow,
) -> std::result::Result<(), PositionError> {
    let Some(position) = row
        .input
        .as_ref()
        .and_then(|input| input.queue.as_ref())
        .and_then(|queue| queue.position.as_ref())
    else {
        return Ok(());
    };
    let expected_slot = position.slot_request_doc_id.clone();
    if expected_slot.is_empty() {
        return Err(InvalidQueuePosition.into());
    }
    let mut current = row.clone();
    let mut visited = rapidhash::RapidHashSet::default();
    loop {
        let current_id = current.doc_id.as_deref().ok_or(InvalidQueuePosition)?;
        if !visited.insert(current_id.to_owned()) {
            return Err(InvalidQueuePosition.into());
        }
        let input = current.input.as_ref().ok_or(InvalidQueuePosition)?;
        let queue = input.queue.as_ref().ok_or(InvalidQueuePosition)?;
        let Some(position) = queue.position.as_ref() else {
            if current_id != expected_slot {
                return Err(InvalidQueuePosition.into());
            }
            return Ok(());
        };
        if !(position.slot_request_doc_id == expected_slot
            && !position.replaces_request_doc_id.is_empty()
            && !visited.contains(&position.replaces_request_doc_id))
        {
            return Err(InvalidQueuePosition.into());
        }
        let response = read
            .execute_read(&format!(
                r#"{{ AgentRequest(filter: {{ _docID: {{ _eq: "{}" }} }}) {{
                {} superseded_by_request_doc_id failure_reason
            }} }}"#,
                escape_graphql_string(&position.replaces_request_doc_id),
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS,
            ))
            .await?;
        let previous: Vec<AgentRequestRow> =
            serde_json::from_value(response["data"]["AgentRequest"].clone())
                .map_err(anyhow::Error::from)?;
        let [previous] = previous.as_slice() else {
            return Err(InvalidQueuePosition.into());
        };
        gents_store::identity::request_receipt::verify_request_receipt_signature(previous)
            .map_err(|_| InvalidQueuePosition)?;
        let old_input = previous.input.as_ref().ok_or(InvalidQueuePosition)?;
        let old_queue = old_input.queue.as_ref().ok_or(InvalidQueuePosition)?;
        let owned = previous.doc_id.as_deref() == Some(position.replaces_request_doc_id.as_str())
            && previous.lifecycle_state == Some(RequestLifecycleState::Superseded)
            && previous.superseded_by_request_doc_id == current.doc_id
            && previous.failure_reason.as_deref() == Some(super::management::REPLACED_REASON)
            && (previous.admission_kind.as_deref() == Some("client")
                || previous.admission_signer_did == previous.requester_did)
            && previous.node_did == current.node_did
            && previous.requester_did == current.requester_did
            && previous.session_id == current.session_id
            && previous.agent_did == current.agent_did
            && previous.workspace_id == current.workspace_id
            && previous.workspace_owner_node_did == current.workspace_owner_node_did
            && previous.workspace_authority == current.workspace_authority
            && previous.workspace_seal_hash == current.workspace_seal_hash
            && same_execution_directory(old_input, input)
            && old_queue.source == QueueSource::User
            && queue.source == QueueSource::User
            && old_queue.policy == QueuePolicy::Append
            && queue.policy == QueuePolicy::Append
            && super::management::replacement_delivery_allowed(old_queue.delivery, queue.delivery);
        if !owned {
            return Err(InvalidQueuePosition.into());
        }
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
    let literal = gents_store::graphql::graphql_string_list_literal(ids.iter().map(String::as_str));
    let response = txn
        .execute(&format!(
            r#"{{ AgentRequest(docIDs: {literal}) {{
            _docID request_id node_did requester_did session_id agent_did input
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
            Err(error) => match error.refusal() {
                Ok(InvalidQueuePosition) => id.clone(),
                Err(error) => return Err(error),
            },
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
        use defra_node::Node as EmbeddedNode;
        use gents_store::config_client::ConfigAccess;

        let node_db = gents_inference::test_support::test_db(
            "effective-slot-batch-uses-only-found-phy",
            crate::test_support::no_lifecycle_plugins(),
        )
        .await;
        let node = node_db.node.clone();

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
                    agent_did: "worker", session_id: "{session_id}", content: "work",
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
        node.shutdown().await.expect("shut down test node");
    }
}
