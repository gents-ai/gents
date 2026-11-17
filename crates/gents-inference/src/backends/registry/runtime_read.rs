use gents_store::config_client::{ConfigApplyTxn, ConfigClientError};
use rapidhash::RapidHashSet;

pub(super) async fn readable_documents(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    doc_ids: &[String],
) -> Result<RapidHashSet<String>, ConfigClientError> {
    if txn.actor_did() != Some(node_did) {
        return Err(ConfigClientError::TransactionActorMismatch);
    }
    if doc_ids.is_empty() {
        return Ok(RapidHashSet::default());
    }
    let subject =
        identity::Did::new(node_did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
    let decisions = txn
        .observe_document_access(
            &subject,
            gents_protocol::schemas::INFERENCE_BACKEND_METADATA_NAME,
            doc_ids,
            defra_node::DocumentAccessMode::Document {
                permission: defra_node::DocumentPermission::Read,
                node_permission: acp::NodePermission::DocumentRead,
            },
        )
        .await?;
    if decisions.len() != doc_ids.len() {
        return Err(ConfigClientError::InvalidOwnerObservation);
    }
    Ok(doc_ids
        .iter()
        .zip(decisions)
        .filter(|(_, decision)| *decision == defra_node::DocumentAccessDecision::Allowed)
        .map(|(doc_id, _)| doc_id.clone())
        .collect())
}
