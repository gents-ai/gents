use anyhow::Result;

use super::ConfigApplyTxn;

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
