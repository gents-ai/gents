use anyhow::{Context, Result};

use super::ConfigApplyTxn;
use crate::Collection;

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
    Ok(())
}
