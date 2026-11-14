use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::document_query::{
    fields::owed_delivery as fields, Create, Filter, Input, Query, Selection,
};
use gents_protocol::owed_delivery::OwedDeliveryRow;

use super::{ConfigAccess, ConfigApplyTxn};

/// Creates the record unless one with its key exists (Lean
/// `Hooks.OwedDelivery.publishOne`). Returns the new document's ID.
async fn create_unless_present(
    txn: &ConfigApplyTxn<'_>,
    runtime: &identity::Did,
    row: &OwedDeliveryRow,
) -> anyhow::Result<Option<String>> {
    txn.validate_runtime_actor(runtime)?;
    let existing = txn
        .run(
            &Query::select(Selection::doc_id())
                .filter(Filter::eq(fields::DELIVERY_KEY, row.delivery_key.as_str()))
                .limit(2)
                .render(),
        )
        .await?;
    if !gents_protocol::graphql::graphql_rows_from_response(&existing, "OwedDelivery").is_empty() {
        return Ok(None);
    }
    let input = Input::from_serialized(serde_json::to_value(row)?);
    let created = txn.run(&Create::<fields::Doc>::new(input).render()).await?;
    Ok(Some(gents_protocol::graphql::extract_mutation_doc_id(
        &created,
        "OwedDelivery",
    )?))
}

/// Publishes one owed delivery as the runtime, in its own transaction, then
/// links its Node parent the way a deferred operation is linked; a record left
/// unlinked is linked by `recover_deferred_node_operations`.
async fn publish(
    access: &ConfigAccess,
    runtime: &identity::Did,
    row: &OwedDeliveryRow,
) -> anyhow::Result<()> {
    let created = access
        .transact_write("hooks.owed_delivery.publish", |txn| {
            Box::pin(create_unless_present(txn, runtime, row))
        })
        .await?;
    let Some(doc_id) = created else {
        return Ok(());
    };
    access
        .transact_write("hooks.owed_delivery.link_parent", |txn| {
            let doc_id = doc_id.as_str();
            Box::pin(async move {
                super::maybe_link_deferred_operation_in_txn(txn, "OwedDelivery", doc_id, runtime)
                    .await
                    .map_err(anyhow::Error::from)
            })
        })
        .await?;
    Ok(())
}

/// Publishes the deliveries a committed transaction owes, as the runtime
/// identity of the node that hosted it. Never fails the committed
/// transaction: its transition committed whatever happens to the record
/// (Lean `Hooks.must_commit_is_decided_by_core`); a record that is not
/// published is reported.
pub(super) async fn publish_owed(node: Option<&Arc<EmbeddedNode>>, rows: Vec<OwedDeliveryRow>) {
    let runtime = node.and_then(|node| {
        node.node_identity_did()
            .and_then(|did| identity::Did::new(did).ok())
            .map(|did| (node, did))
    });
    let Some((node, runtime)) = runtime else {
        for row in &rows {
            tracing::warn!(
                point = row.point,
                plugin = row.plugin,
                subject = row.subject_id,
                "an owed lifecycle delivery cannot be recorded outside its hosting runtime"
            );
        }
        return;
    };
    let access = ConfigAccess::local_as(Arc::clone(node), runtime.clone());
    for mut row in rows {
        row.node_did = runtime.as_str().to_owned();
        let row = &row;
        if let Err(error) = publish(&access, &runtime, row).await {
            tracing::warn!(
                point = row.point,
                plugin = row.plugin,
                subject = row.subject_id,
                error = %gents_protocol::error_chain::ErrorChain(error.as_ref()),
                "an owed lifecycle delivery was not recorded"
            );
        }
    }
}
