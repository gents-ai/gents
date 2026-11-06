use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use super::apply_txn::EmbeddedTransactionPurpose;
use super::transact::{expect_committed, transact_owned_with_finality, TransactionMode};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError, Result, TransactionFailure};
use crate::store::config_client::client_pack_evidence::{
    apply_pack_evidence, verify_pack_evidence, PackEvidence, PackEvidenceError, PackMutation,
};
use crate::store::config_client::write_telemetry::WriteBackend;

/// A User transaction exposes reads and exact verified client graphs only.
pub struct VerifiedPackTxn<'txn, 'access> {
    inner: &'txn ConfigApplyTxn<'access>,
}

#[derive(Debug, thiserror::Error)]
#[error("verified client transaction failed: {error}")]
pub struct VerifiedPackTransactionError<E> {
    pub error: E,
    pub unconfirmed: bool,
}

impl VerifiedPackTxn<'_, '_> {
    pub async fn read(&self, document: &str) -> Result<Value> {
        super::graphql::ensure_query_document(document)?;
        self.inner.execute(document).await
    }

    pub async fn apply(
        &self,
        mutation: &PackMutation,
        evidence: PackEvidence,
    ) -> std::result::Result<Value, PackEvidenceError> {
        let actor = self.inner.actor_did().ok_or(PackEvidenceError::Mismatch)?;
        let graph = verify_pack_evidence(self.inner, actor, mutation, evidence).await?;
        apply_pack_evidence(self.inner, mutation, graph).await
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
