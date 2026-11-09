//! Lean `Identity.OwnerStaged`: an owner-managed relation that a
//! configuration change implies is staged by the document's registered owner.
//! Native refuses a node administrator that does not manage the document on
//! every public route, so Gents never relies on that admission: the actor
//! stages the change when it owns the document; otherwise the embedding
//! runtime stages it as the owner, in the same transaction, for a requester
//! that [`OwnerStagedAuthority`] admits. Over HTTP no runtime participates, so
//! the requester's own change is sent and native ACP decides it.

use std::sync::Arc;

use super::apply_txn::TxnBackend;
use super::execute::native_relationship_error;
use super::native::execute_owned_native;
use super::{ConfigApplyTxn, ConfigClientError, Result};
use crate::store::graphql::escape_graphql_string;

/// What the requester must natively hold before the owner stages a change for
/// it (`Identity.OwnerStaged.mayRequest` and the pairing-reader authority).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerStagedAuthority {
    /// `update` on the changed document itself.
    DocumentUpdate,
    /// `update` on the runtime's Node, as client-requested runtime operations
    /// are authorized; used where the document has no reader manager yet.
    NodeUpdate,
}

impl ConfigApplyTxn<'_> {
    /// Stage an owner-managed actor relation (`Identity.OwnerStaged.ownerStaged`).
    pub async fn change_owner_staged_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
        delete: bool,
        authority: OwnerStagedAuthority,
    ) -> Result<()> {
        self.change_owner_staged(
            collection,
            doc_id,
            relation,
            target.as_str(),
            delete,
            authority,
        )
        .await
    }

    /// Stage an owner-managed relation to a native entity set.
    pub async fn change_owner_staged_entity_set_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        (target_resource, target_doc_id, target_relation): (&str, &str, &str),
        delete: bool,
        authority: OwnerStagedAuthority,
    ) -> Result<()> {
        let target = super::super::relationships::entity_set_target(
            target_resource,
            target_doc_id,
            target_relation,
        )?;
        self.change_owner_staged(collection, doc_id, relation, &target, delete, authority)
            .await
    }

    async fn change_owner_staged(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
        authority: OwnerStagedAuthority,
    ) -> Result<()> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        if self.has_pack_interpreter() || matches!(self.backend, TxnBackend::Http { .. }) {
            return self
                .change_document_relationship(collection, doc_id, relation, target, delete)
                .await;
        }
        let actor = self
            .actor_did()
            .ok_or(ConfigClientError::RelationshipActorRequired)?
            .to_owned();
        let owner = self
            .document_owners(collection, &[doc_id.to_owned()])
            .await?
            .into_iter()
            .find(|observation| observation.doc_id == doc_id)
            .and_then(|observation| observation.owner);
        if owner.as_ref().is_some_and(|owner| owner.as_str() == actor) {
            return self
                .change_document_relationship(collection, doc_id, relation, target, delete)
                .await;
        }
        let refused = |kind: fn(String, String) -> ConfigClientError| {
            Err(kind(collection.to_owned(), doc_id.to_owned()))
        };
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::LocalAccessRequired);
        };
        let unmanaged = |collection, doc_id| ConfigClientError::OwnerStagedRelationshipUnmanaged {
            collection,
            doc_id,
        };
        let Some(owner) = owner else {
            return refused(unmanaged);
        };
        let runtime = node
            .node_identity_did()
            .ok_or(ConfigClientError::LocalAccessRequired)?
            .to_owned();
        if owner.as_str() != runtime && !self.configures_agent(&runtime, &owner).await? {
            return refused(unmanaged);
        }
        let requester = identity
            .as_ref()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        if !self
            .owner_staged_request_allowed(requester, &runtime, collection, doc_id, authority)
            .await?
        {
            return refused(|collection, doc_id| {
                ConfigClientError::OwnerStagedRelationshipDenied { collection, doc_id }
            });
        }
        let context = defra_core::RequestContext::default().with_identity(Some(owner.to_string()));
        let node = Arc::clone(node);
        let handle = handle.clone();
        let (collection, doc_id, relation, target) = (
            collection.to_owned(),
            doc_id.to_owned(),
            relation.to_owned(),
            target.to_owned(),
        );
        execute_owned_native(
            async move {
                if delete {
                    node.delete_dac_actor_relationship_for_in_txn(
                        &handle,
                        &context,
                        &collection,
                        &doc_id,
                        &relation,
                        &target,
                    )
                    .await
                } else {
                    node.add_dac_actor_relationship_for_in_txn(
                        &handle,
                        &context,
                        &collection,
                        &doc_id,
                        &relation,
                        &target,
                    )
                    .await
                }
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(native_relationship_error)?;
        Ok(())
    }

    async fn owner_staged_request_allowed(
        &self,
        requester: &identity::Did,
        runtime: &str,
        collection: &str,
        doc_id: &str,
        authority: OwnerStagedAuthority,
    ) -> Result<bool> {
        let (collection, doc_id) = match authority {
            OwnerStagedAuthority::DocumentUpdate => (collection.to_owned(), doc_id.to_owned()),
            OwnerStagedAuthority::NodeUpdate => {
                let response = self
                    .execute(&format!(
                        r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
                        escape_graphql_string(runtime),
                    ))
                    .await?;
                let nodes = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
                let [node] = nodes.as_slice() else {
                    return Ok(false);
                };
                let Some(id) = node["_docID"].as_str().filter(|id| !id.is_empty()) else {
                    return Ok(false);
                };
                ("Node".to_owned(), id.to_owned())
            }
        };
        let decisions = self
            .observe_document_access(
                requester,
                &collection,
                &[doc_id],
                defra_node::DocumentAccessMode::Named {
                    permission: "update".into(),
                    node_permission: acp::nac::NodePermission::DocumentUpdate,
                },
            )
            .await?;
        Ok(matches!(
            decisions.as_slice(),
            [defra_node::DocumentAccessDecision::Allowed]
        ))
    }

    /// Whether `agent` is an Agent configured on the runtime's Node, as the
    /// requesting actor observes it in this transaction.
    async fn configures_agent(&self, runtime: &str, agent: &identity::Did) -> Result<bool> {
        let response = self
            .execute(&format!(
                r#"{{ Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 1) {{_docID}} }}"#,
                escape_graphql_string(runtime),
                escape_graphql_string(agent.as_str()),
            ))
            .await?;
        Ok(response["data"]["Agent"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()))
    }
}

#[cfg(test)]
mod tests;
