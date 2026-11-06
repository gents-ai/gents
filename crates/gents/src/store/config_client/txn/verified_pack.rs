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
    pub(super) inner: &'txn ConfigApplyTxn<'access>,
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
    pub(crate) fn interpreter(&self) -> &ConfigApplyTxn<'_> {
        self.inner
    }

    /// Use the desired-state interpreter only while its effects require the
    /// client approver bound to this native transaction.
    pub fn approved_interpreter(&self) -> Result<&ConfigApplyTxn<'_>> {
        if !self.inner.has_pack_interpreter() {
            return Err(ConfigClientError::TransactionAuthorizationUnsupported);
        }
        Ok(self.inner)
    }
    pub async fn read(&self, document: &str) -> Result<Value> {
        self.read_with_variables(document, &serde_json::json!({}))
            .await
    }

    pub async fn read_with_variables(&self, document: &str, variables: &Value) -> Result<Value> {
        super::graphql::ensure_query_document(document)?;
        self.inner.execute_with_variables(document, variables).await
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
        expected: &crate::store::config_client::PackRelationship,
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
        if &relationship != expected {
            return Err(PackEvidenceError::Mismatch);
        }
        if super::super::client_signed_agent::trusted_delete_schema(&relationship.collection)
            .is_none()
        {
            return Err(PackEvidenceError::UnsupportedCollection);
        }
        let target = match &relationship.target {
            PackRelationshipTarget::Actor { did } => identity::Did::new(did)
                .map_err(ConfigClientError::RelationshipTargetIdentity)?
                .to_string(),
            PackRelationshipTarget::Resource { resource, doc_id } => {
                super::super::relationships::resource_target(resource, doc_id)?
            }
            PackRelationshipTarget::EntitySet {
                resource,
                doc_id,
                relation,
            } => super::super::relationships::entity_set_target(resource, doc_id, relation)?,
        };
        self.inner
            .change_document_relationship_unchecked(
                &relationship.collection,
                &relationship.doc_id,
                &relationship.relation,
                &target,
                relationship.delete,
            )
            .await?;
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
        self.transact_verified_pack_inner(operation, None, callback)
            .await
    }

    pub async fn transact_verified_pack_with_approver<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        scope: PackApprovalScope,
        approver: Arc<dyn super::super::PackEffectApprover>,
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
        let approval = super::super::pack_effect::PackInterpreterApproval { scope, approver };
        self.transact_verified_pack_inner(operation, Some(Arc::new(approval)), callback)
            .await
    }

    async fn transact_verified_pack_inner<'a, T, E, F>(
        &'a self,
        operation: &'static str,
        approval: Option<Arc<super::super::pack_effect::PackInterpreterApproval>>,
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
        let mode = if approval.is_some() {
            TransactionMode::SingleAttempt
        } else {
            TransactionMode::ConflictRetry
        };
        let callback = Arc::new(callback);
        let result = transact_owned_with_finality(
            operation,
            WriteBackend::Embedded,
            mode,
            move |operation, rollback| {
                let actor = actor.clone();
                let approval = approval.clone();
                Box::pin(async move {
                    let mut txn = ConfigApplyTxn::begin_local_owned(
                        local.node(),
                        Some(actor),
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::VerifiedClient,
                    )
                    .await?;
                    txn.pack_approval = approval;
                    Ok(txn)
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
