//! Fixture relationship changes made the way production owners make them: by
//! the acting identity inside a native transaction. Outside one, native
//! relationship changes also require node `get-collection`, which an Agent
//! (and a fixture User granted only its scenario's permissions) does not hold.

use crate::store::config_client::ConfigClientError;
use crate::ConfigAccess;

/// Grant (`delete == false`) or revoke an actor relationship as `access`'s actor.
pub(crate) async fn actor_in_txn(
    access: &ConfigAccess,
    collection: &str,
    doc_id: &str,
    relation: &str,
    target: &identity::Did,
    delete: bool,
) -> Result<(), ConfigClientError> {
    access
        .transact_write("test.relationship.actor", |txn| {
            Box::pin(async move {
                if delete {
                    txn.delete_actor_relationship(collection, doc_id, relation, target)
                        .await
                } else {
                    txn.add_actor_relationship(collection, doc_id, relation, target)
                        .await
                }
            })
        })
        .await
}

/// Grant (`delete == false`) or revoke a resource relationship as `access`'s actor.
pub(crate) async fn resource_in_txn(
    access: &ConfigAccess,
    collection: &str,
    doc_id: &str,
    relation: &str,
    target_resource: &str,
    target_doc_id: &str,
    delete: bool,
) -> Result<(), ConfigClientError> {
    access
        .transact_write("test.relationship.resource", |txn| {
            Box::pin(async move {
                if delete {
                    txn.delete_resource_relationship(
                        collection,
                        doc_id,
                        relation,
                        target_resource,
                        target_doc_id,
                    )
                    .await
                } else {
                    txn.add_resource_relationship(
                        collection,
                        doc_id,
                        relation,
                        target_resource,
                        target_doc_id,
                    )
                    .await
                }
            })
        })
        .await
}
