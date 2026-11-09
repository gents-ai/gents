use anyhow::{Context, Result};

use super::ConfigApplyTxn;
use crate::Collection;

/// Publish exact Node manager usersets only while creating their document.
pub(crate) async fn publish_node_managers_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    manager_relation: &str,
    node_doc_id: &str,
) -> Result<()> {
    for relation in ["owner", "administrator"] {
        txn.add_entity_set_relationship(
            collection,
            doc_id,
            manager_relation,
            "nodes",
            node_doc_id,
            relation,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn publish_secret_configuration_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    node_did: &str,
) -> Result<()> {
    publish_node_child_in_txn(txn, collection, doc_id, node_did)
        .await
        .context("credential publication")?;
    txn.add_actor_relationship(
        collection,
        doc_id,
        "runtime_writer",
        &identity::Did::new(node_did)?,
    )
    .await?;
    Ok(())
}

pub(crate) async fn publish_node_child_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    doc_id: &str,
    node_did: &str,
) -> Result<()> {
    let (parent, _) =
        super::desired_state::read::read_record(txn, Collection::Node, node_did, node_did)
            .await?
            .context("document publication requires a visible Node")?;
    let owners = txn
        .document_owner_registration("Node", std::slice::from_ref(&parent))
        .await?;
    anyhow::ensure!(
        owners.len() == 1
            && owners[0].doc_id == parent
            && owners[0].visible
            && owners[0].registered,
        "document publication requires a registered Node owner"
    );
    txn.add_resource_relationship(collection, doc_id, "parent", "nodes", &parent)
        .await?;
    if gents_protocol::schemas::identity_policy_resource(collection) == Some("configuration") {
        publish_node_managers_in_txn(txn, collection, doc_id, "selection_manager", &parent).await?;
    }
    Ok(())
}
