//! Lean `Identity.OwnerStaged`: an owner-managed relation that a
//! configuration change implies is staged by the document's registered owner.
//! Native refuses a node administrator that does not manage the document on
//! every public route, so Gents never relies on that admission: the actor
//! stages the change when it owns the document; otherwise the embedding
//! runtime stages it as the owner, in the same transaction, for a requester
//! that the modeled authority admits. A pairing reader is decided only in an
//! embedded transaction; a client reaches it through the runtime's signed
//! route. A document-update change over HTTP still sends the requester's own
//! change and native ACP decides it.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use super::apply_txn::{EmbeddedTransactionPurpose, TxnBackend};
use super::execute::native_relationship_error;
use super::native::execute_owned_native;
use super::transact::{expect_committed, transact_owned, TransactionMode};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError, Result};
use crate::store::config_client::pairing_reader::reader_relation;
use crate::store::config_client::write_telemetry::WriteBackend;
use crate::store::config_client::{PairingObservation, PairingReadReason};
use crate::store::graphql::escape_graphql_string;

/// What the requester must natively hold before the owner stages a change for
/// it (`Identity.OwnerStaged.Authority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authority {
    /// `update` on the changed document itself.
    DocumentUpdate,
    /// `update` on the runtime's own Node row, only for a pairing
    /// observation; the runtime stages only one that it owns.
    PairingNode,
}

impl ConfigApplyTxn<'_> {
    /// Stage an owner-managed actor relation (`Identity.OwnerStaged.outcome`)
    /// for a requester that may update the document.
    pub(crate) async fn change_owner_staged_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
        delete: bool,
    ) -> Result<()> {
        self.change_owner_staged(
            collection,
            doc_id,
            relation,
            target.as_str(),
            delete,
            Authority::DocumentUpdate,
        )
        .await
    }

    /// Stage an owner-managed relation to a native entity set for a requester
    /// that may update the document.
    pub(crate) async fn change_owner_staged_entity_set_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        (target_resource, target_doc_id, target_relation): (&str, &str, &str),
        delete: bool,
    ) -> Result<()> {
        let target = super::super::relationships::entity_set_target(
            target_resource,
            target_doc_id,
            target_relation,
        )?;
        self.change_owner_staged(
            collection,
            doc_id,
            relation,
            &target,
            delete,
            Authority::DocumentUpdate,
        )
        .await
    }

    /// Stage a pairing reader for a requester that may update the runtime's
    /// own Node (`Identity.OwnerStaged.runtimeNode`). Only a pairing
    /// observation the runtime owns is staged.
    pub(in crate::store::config_client) async fn change_owner_staged_pairing_reader(
        &self,
        observation: PairingObservation,
        doc_id: &str,
        reason: PairingReadReason,
        reader: &identity::Did,
        delete: bool,
    ) -> Result<()> {
        self.change_owner_staged(
            observation.collection(),
            doc_id,
            reader_relation(reason),
            reader.as_str(),
            delete,
            Authority::PairingNode,
        )
        .await
    }

    async fn change_owner_staged(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
        authority: Authority,
    ) -> Result<()> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        if self.has_pack_interpreter()
            || (authority == Authority::DocumentUpdate
                && matches!(self.backend, TxnBackend::Http { .. }))
        {
            return self
                .change_document_relationship(collection, doc_id, relation, target, delete)
                .await;
        }
        let refused = |kind: fn(String, String) -> ConfigClientError| {
            Err(kind(collection.to_owned(), doc_id.to_owned()))
        };
        let unmanaged = |collection, doc_id| ConfigClientError::OwnerStagedRelationshipUnmanaged {
            collection,
            doc_id,
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
        let runtime = identity::Did::new(
            node.node_identity_did()
                .ok_or(ConfigClientError::LocalAccessRequired)?,
        )
        .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let requester = identity
            .as_ref()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        // Authority first: a requester that may not request the change learns
        // nothing about whether the document exists or who owns it
        // (`unauthorized_refusal_independent_of_document`).
        if !self
            .owner_staged_request_allowed(requester, &runtime, collection, doc_id, authority)
            .await?
        {
            return refused(|collection, doc_id| {
                ConfigClientError::OwnerStagedRelationshipDenied { collection, doc_id }
            });
        }
        // `foundOwner`: as the requester observes it, else as the runtime does,
        // so an authorized requester may change a runtime-owned document it
        // cannot read yet.
        let mut owner = None;
        for reader in [requester, &runtime] {
            owner = self
                .document_owners_as(reader, collection, &[doc_id.to_owned()])
                .await?
                .into_iter()
                .find(|observation| observation.doc_id == doc_id)
                .and_then(|observation| observation.owner);
            if owner.is_some() {
                break;
            }
        }
        let Some(owner) = owner else {
            return refused(unmanaged);
        };
        if &owner == requester {
            return self
                .change_document_relationship(collection, doc_id, relation, target, delete)
                .await;
        }
        // `stagesAs`: an embedded transaction signs only as the runtime or one
        // of its Agents, and the pairing authority reaches only observations
        // the runtime itself owns.
        let managed = match authority {
            Authority::DocumentUpdate => {
                owner == runtime || self.configures_agent(&runtime, &owner).await?
            }
            Authority::PairingNode => owner == runtime,
        };
        if !managed {
            return refused(unmanaged);
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
        runtime: &identity::Did,
        collection: &str,
        doc_id: &str,
        authority: Authority,
    ) -> Result<bool> {
        let (collection, doc_id) = match authority {
            Authority::DocumentUpdate => (collection.to_owned(), doc_id.to_owned()),
            Authority::PairingNode => match self.runtime_node_doc_id(runtime).await? {
                Some(node) => ("Node".to_owned(), node),
                None => return Ok(false),
            },
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

    /// The single Node row the runtime reads for its own `node_did` in this
    /// transaction; the requester's view never chooses it. A local write keeps
    /// `node_did` unique against every live row whatever its writer may read,
    /// but a replicated merge can keep a second, unindexed row, so more than
    /// one row resolves to none rather than to either.
    async fn runtime_node_doc_id(&self, runtime: &identity::Did) -> Result<Option<String>> {
        let response = self
            .execute_local_response_with_actor(
                &format!(
                    r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
                    escape_graphql_string(runtime.as_str()),
                ),
                Some(runtime.clone()),
                None,
            )
            .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("Node"))
            .and_then(Value::as_array);
        Ok(match rows.map(Vec::as_slice) {
            Some([row]) => row["_docID"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            _ => None,
        })
    }

    /// Whether `agent` is an Agent configured on the runtime's Node, as the
    /// runtime observes it in this transaction (`stagesAs`): the requester's
    /// read access never decides which owners the runtime manages.
    async fn configures_agent(
        &self,
        runtime: &identity::Did,
        agent: &identity::Did,
    ) -> Result<bool> {
        let response = self
            .execute_local_response_with_actor(
                &format!(
                    r#"{{ Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 1) {{_docID}} }}"#,
                    escape_graphql_string(runtime.as_str()),
                    escape_graphql_string(agent.as_str()),
                ),
                Some(runtime.clone()),
                None,
            )
            .await?;
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get("Agent"))
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()))
    }
}

impl ConfigAccess {
    /// Begin a transaction as the non-runtime client this access names, for
    /// an operation the runtime authenticated from that client's signed
    /// command. Only the pairing-reader decision runs in it.
    pub(in crate::store::config_client) async fn transact_verified_client_pairing_reader<'a, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<()>
    where
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<()>> + Send + 'a,
    {
        let Self::Local(local) = self else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let actor = local
            .identity()
            .cloned()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        if local.node().node_identity_did() == Some(actor.as_str()) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                let actor = actor.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        local.node(),
                        Some(actor),
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::VerifiedClient,
                    )
                    .await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }
}

#[cfg(test)]
mod tests;
