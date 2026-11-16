use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use crate::graphql::escape_graphql_string;

const DEFERRED_COLLECTIONS: [&str; 6] = [
    "EnrollmentOperatorNonce",
    "EnrollmentRenewalPolicy",
    "NetworkAdminPin",
    "OwedDelivery",
    "PeerPairingApplied",
    "PeerPairingDesired",
];

#[derive(Debug, thiserror::Error)]
pub enum DeferredOperationError {
    #[error("{0}")]
    Config(#[from] ConfigClientError),
    #[error(transparent)]
    Graphql(#[from] gents_protocol::graphql::GraphqlError),
    #[error("authenticated actor cannot update the local Node")]
    NodeUpdateDenied,
    #[error("unsupported deferred operation collection")]
    UnsupportedCollection,
    #[error("deferred operation is not uniquely visible")]
    DocumentUnavailable,
    #[error("deferred operation has no matching native runtime owner")]
    OwnerMismatch,
    #[error("deferred operation has no publication marker")]
    MissingMarker,
    #[error("deferred operation Node is ambiguous")]
    AmbiguousNode,
    #[error("deferred operation Node has no document ID")]
    MissingNodeId,
    #[error("deferred operation Node has no registered owner")]
    MissingNodeOwner,
    #[error("deferred operation changed before its Node link was recorded")]
    ChangedBeforeLink,
}

/// Authorize a client-requested runtime operation against the native Node ACP.
pub async fn require_node_update_in_txn(
    txn: &ConfigApplyTxn<'_>,
    runtime: &identity::Did,
    actor: &identity::Did,
) -> std::result::Result<(), DeferredOperationError> {
    txn.validate_runtime_actor(runtime)?;
    let did = escape_graphql_string(runtime.as_str());
    let response = txn
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}} }}"#
        ))
        .await?;
    let nodes = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
    let [node] = nodes.as_slice() else {
        return Err(DeferredOperationError::AmbiguousNode);
    };
    let id = node["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(DeferredOperationError::MissingNodeId)?
        .to_owned();
    let decisions = txn
        .observe_document_access(
            actor,
            "Node",
            &[id],
            defra_node::DocumentAccessMode::Named {
                permission: "update".into(),
                node_permission: acp::nac::NodePermission::DocumentUpdate,
            },
        )
        .await?;
    if !matches!(
        decisions.as_slice(),
        [defra_node::DocumentAccessDecision::Allowed]
    ) {
        return Err(DeferredOperationError::NodeUpdateDenied);
    }
    Ok(())
}

/// Publish the Node parent only in the transaction that marks the relationship
/// complete. A missing visible Node leaves the new runtime-owned document
/// private and pending; a completed marker is never used to restore a grant.
pub async fn maybe_link_deferred_operation_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    runtime: &identity::Did,
) -> std::result::Result<bool, DeferredOperationError> {
    if !DEFERRED_COLLECTIONS.contains(&collection) {
        return Err(DeferredOperationError::UnsupportedCollection);
    }
    txn.validate_runtime_actor(runtime)?;
    let escaped_doc = escape_graphql_string(doc_id);
    let response = txn.execute(&format!(
        r#"{{ {collection}(filter: {{_docID: {{_eq: "{escaped_doc}"}}}}, limit: 2) {{_docID parent_linked}} }}"#
    )).await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, collection);
    let [row] = rows.as_slice() else {
        return Err(DeferredOperationError::DocumentUnavailable);
    };
    let owners = txn
        .document_owners(collection, &[doc_id.to_owned()])
        .await?;
    if owners.len() != 1 || !owners[0].visible || owners[0].owner.as_ref() != Some(runtime) {
        return Err(DeferredOperationError::OwnerMismatch);
    }
    if row["parent_linked"]
        .as_bool()
        .ok_or(DeferredOperationError::MissingMarker)?
    {
        return Ok(false);
    }
    let did = escape_graphql_string(runtime.as_str());
    let response = txn
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}} }}"#
        ))
        .await?;
    let nodes = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
    if nodes.is_empty() {
        return Ok(false);
    }
    let [node] = nodes.as_slice() else {
        return Err(DeferredOperationError::AmbiguousNode);
    };
    let parent = node["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or(DeferredOperationError::MissingNodeId)?
        .to_owned();
    let owners = txn
        .document_owners("Node", std::slice::from_ref(&parent))
        .await?;
    if owners.len() != 1 || !owners[0].visible || owners[0].owner.is_none() {
        return Err(DeferredOperationError::MissingNodeOwner);
    }
    txn.add_resource_relationship(collection, doc_id, "parent", "nodes", &parent)
        .await?;
    if matches!(collection, "PeerPairingApplied" | "PeerPairingDesired") {
        let node_owner = owners[0]
            .owner
            .as_ref()
            .ok_or(DeferredOperationError::MissingNodeOwner)?;
        txn.add_actor_relationship(collection, doc_id, "reader_manager", node_owner)
            .await?;
    }
    let response = txn.execute(&format!(
        r#"mutation {{ update_{collection}(docID: "{escaped_doc}", filter: {{parent_linked: {{_eq: false}}}}, input: {{node_doc_id: "{}", parent_linked: true}}) {{_docID}} }}"#,
        escape_graphql_string(&parent),
    )).await?;
    if gents_protocol::graphql::extract_mutation_doc_id(&response, collection)? != doc_id {
        return Err(DeferredOperationError::ChangedBeforeLink);
    }
    Ok(true)
}

pub async fn recover_deferred_node_operations(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
) -> Result<usize> {
    anyhow::ensure!(
        node.node_identity_did() == Some(node_did),
        "deferred operation recovery requires the configured runtime identity"
    );
    let runtime = identity::Did::new(node_did)?;
    let access = ConfigAccess::local_as(node.clone(), runtime.clone());
    let mut repaired = 0;
    for collection in DEFERRED_COLLECTIONS {
        let response = access
            .execute(&format!(
                r#"{{ {collection}(filter: {{parent_linked: {{_eq: false}}}}) {{_docID}} }}"#
            ))
            .await?;
        for row in gents_protocol::graphql::graphql_rows_from_response(&response, collection) {
            let doc = row["_docID"]
                .as_str()
                .filter(|id| !id.is_empty())
                .context("pending deferred operation has no document ID")?
                .to_owned();
            let linked = access
                .transact_write("node.recover_deferred_operation", |txn| {
                    let runtime = &runtime;
                    let doc = &doc;
                    Box::pin(async move {
                        Ok::<_, anyhow::Error>(
                            maybe_link_deferred_operation_in_txn(txn, collection, doc, runtime)
                                .await?,
                        )
                    })
                })
                .await?;
            repaired += usize::from(linked);
        }
    }
    Ok(repaired)
}
