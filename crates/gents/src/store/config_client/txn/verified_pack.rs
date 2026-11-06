use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use super::apply_txn::{EmbeddedTransactionPurpose, TxnBackend};
use super::transact::{expect_committed, transact_owned_with_finality, TransactionMode};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError, Result, TransactionFailure};
use crate::store::config_client::client_pack_evidence::{
    apply_pack_evidence, verify_pack_evidence, verify_pack_relationship, PackApprovalScope,
    PackEvidence, PackEvidenceError, PackMutation, PackRelationshipEvidence,
    PackRelationshipTarget,
};
use crate::store::config_client::write_telemetry::WriteBackend;

/// A User transaction exposes reads and client-approved effects only.
pub struct VerifiedPackTxn<'txn, 'access> {
    inner: &'txn ConfigApplyTxn<'access>,
}

#[derive(Debug, thiserror::Error)]
#[error("verified client transaction failed: {error}")]
pub struct VerifiedPackTransactionError<E> {
    pub error: E,
    pub unconfirmed: bool,
}

impl<E> TransactionFailure for VerifiedPackTransactionError<E>
where
    E: TransactionFailure,
{
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        self.error.config_client_error()
    }
}

impl VerifiedPackTxn<'_, '_> {
    pub async fn read(&self, document: &str) -> Result<Value> {
        super::graphql::ensure_query_document(document)?;
        self.inner.execute(document).await
    }

    pub async fn apply(
        &self,
        scope: &PackApprovalScope,
        mutation: &PackMutation,
        evidence: PackEvidence,
    ) -> std::result::Result<Value, PackEvidenceError> {
        self.require_scope_node(scope)?;
        let actor = self.inner.actor_did().ok_or(PackEvidenceError::Mismatch)?;
        let graph = verify_pack_evidence(self.inner, actor, scope, mutation, evidence).await?;
        apply_pack_evidence(self.inner, mutation, graph).await
    }

    /// Apply one User-approved relationship under the native User ACP check.
    pub async fn apply_relationship(
        &self,
        scope: &PackApprovalScope,
        evidence: PackRelationshipEvidence,
    ) -> std::result::Result<(), PackEvidenceError> {
        self.require_scope_node(scope)?;
        let actor = self
            .inner
            .actor_did()
            .ok_or(PackEvidenceError::Mismatch)?
            .to_owned();
        let scope = scope.clone();
        let relationship =
            tokio::task::spawn_blocking(move || verify_pack_relationship(&actor, &scope, evidence))
                .await??;
        if super::super::client_signed_agent::trusted_delete_schema(&relationship.collection)
            .is_none()
        {
            return Err(PackEvidenceError::UnsupportedCollection);
        }
        let PackRelationshipTarget::Actor { did } = &relationship.target else {
            return self.apply_resource_relationship(relationship).await;
        };
        let target =
            identity::Did::new(did).map_err(ConfigClientError::RelationshipTargetIdentity)?;
        if relationship.delete {
            self.inner
                .delete_actor_relationship(
                    &relationship.collection,
                    &relationship.doc_id,
                    &relationship.relation,
                    &target,
                )
                .await?;
        } else {
            self.inner
                .add_actor_relationship(
                    &relationship.collection,
                    &relationship.doc_id,
                    &relationship.relation,
                    &target,
                )
                .await?;
        }
        Ok(())
    }

    fn require_scope_node(
        &self,
        scope: &PackApprovalScope,
    ) -> std::result::Result<(), PackEvidenceError> {
        match &self.inner.backend {
            TxnBackend::Embedded { node, .. }
                if node.node_identity_did() == Some(scope.node_did.as_str()) =>
            {
                Ok(())
            }
            _ => Err(PackEvidenceError::Mismatch),
        }
    }

    async fn apply_resource_relationship(
        &self,
        relationship: crate::store::config_client::client_pack_evidence::PackRelationship,
    ) -> std::result::Result<(), PackEvidenceError> {
        let collection = &relationship.collection;
        let doc = &relationship.doc_id;
        let relation = &relationship.relation;
        match (&relationship.target, relationship.delete) {
            (PackRelationshipTarget::Resource { resource, doc_id }, false) => {
                self.inner
                    .add_resource_relationship(collection, doc, relation, resource, doc_id)
                    .await?
            }
            (PackRelationshipTarget::Resource { resource, doc_id }, true) => {
                self.inner
                    .delete_resource_relationship(collection, doc, relation, resource, doc_id)
                    .await?
            }
            (
                PackRelationshipTarget::EntitySet {
                    resource,
                    doc_id,
                    relation: target_relation,
                },
                false,
            ) => {
                self.inner
                    .add_entity_set_relationship(
                        collection,
                        doc,
                        relation,
                        resource,
                        doc_id,
                        target_relation,
                    )
                    .await?
            }
            (
                PackRelationshipTarget::EntitySet {
                    resource,
                    doc_id,
                    relation: target_relation,
                },
                true,
            ) => {
                self.inner
                    .delete_entity_set_relationship(
                        collection,
                        doc,
                        relation,
                        resource,
                        doc_id,
                        target_relation,
                    )
                    .await?
            }
            (PackRelationshipTarget::Actor { .. }, _) => return Err(PackEvidenceError::Mismatch),
        }
        Ok(())
    }
}

impl ConfigAccess {
    /// Run only verified client graphs under the selected non-Node actor.
    /// The unconfirmed flag reports native commit finality, not a client receipt.
    pub async fn transact_verified_pack<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> std::result::Result<T, VerifiedPackTransactionError<E>>
    where
        T: Send + 'a,
        E: TransactionFailure + From<ConfigClientError> + Send + 'a,
        F: for<'txn> Fn(&'txn VerifiedPackTxn<'txn, 'a>) -> BoxFuture<'txn, Result<T, E>>
            + Send
            + Sync
            + 'a,
    {
        let Self::Local(local) = self else {
            return Err(VerifiedPackTransactionError {
                error: E::from(ConfigClientError::EmbeddedAccessRequired),
                unconfirmed: false,
            });
        };
        let Some(actor) = local.identity().cloned() else {
            return Err(VerifiedPackTransactionError {
                error: E::from(ConfigClientError::RelationshipActorRequired),
                unconfirmed: false,
            });
        };
        if local.node().node_identity_did() == Some(actor.as_str()) {
            return Err(VerifiedPackTransactionError {
                error: E::from(ConfigClientError::TransactionActorMismatch),
                unconfirmed: false,
            });
        }
        let unconfirmed = Arc::new(AtomicBool::new(false));
        let callback = Arc::new(callback);
        let result = transact_owned_with_finality(
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
            move |txn| {
                let callback = Arc::clone(&callback);
                Box::pin(async move {
                    let verified = VerifiedPackTxn { inner: txn };
                    callback(&verified).await
                })
            },
            Some(unconfirmed.clone()),
        )
        .await;
        result
            .map(expect_committed)
            .map_err(|error| VerifiedPackTransactionError {
                error,
                unconfirmed: unconfirmed.load(Ordering::Acquire),
            })
    }
}
