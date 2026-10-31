use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use defra_node::EmbeddedNode;
use identity::Did;
use query::TransactionHandle;

use super::gate::mutation_write_gate;
use super::native::NativeOperationTracker;
#[cfg(test)]
use super::rollback::RollbackDropCleanupPause;
use super::rollback::{begin_embedded_owned, begin_http_owned, RollbackOnDrop};
use super::{ConfigClientError, GraphqlEndpoint, Result};
use crate::store::config_client::write_telemetry::WriteOperation;

pub(super) enum TxnBackend<'a> {
    Http {
        endpoint: &'a GraphqlEndpoint,
        id: String,
        client: reqwest::Client,
    },
    Embedded {
        node: Arc<EmbeddedNode>,
        handle: TransactionHandle,
        identity: Option<Did>,
        native_operations: Arc<NativeOperationTracker>,
    },
}

/// An open explicit transaction, handed to [`ConfigAccess::transact_write`] callbacks.
/// Raw begin/commit/discard transport lives in the crate-private `graphql`
/// submodule; external callers must use the owned entry points.
pub struct ConfigApplyTxn<'a> {
    pub(super) backend: TxnBackend<'a>,
    pub(super) rollback_on_drop: Option<RollbackOnDrop>,
    pub(super) affected_documents: AtomicU64,
    pub(super) operation: Option<&'static str>,
    pub(super) contends_with_request_siblings: AtomicBool,
}

pub(super) enum EmbeddedTransactionPurpose {
    Mutation,
    ExecutionRenewal(Arc<NativeOperationTracker>),
    ReadOnly,
}

impl<'a> ConfigApplyTxn<'a> {
    pub(super) fn embedded_poisoned(&self) -> bool {
        matches!(
            &self.backend,
            TxnBackend::Embedded { native_operations, .. } if native_operations.is_abandoned()
        )
    }

    pub(super) async fn begin_local_owned<'txn>(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: WriteOperation,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        purpose: EmbeddedTransactionPurpose,
    ) -> Result<ConfigApplyTxn<'txn>> {
        let read_only = matches!(&purpose, EmbeddedTransactionPurpose::ReadOnly);
        let native_gate = mutation_write_gate(node);
        let write_guard = match &purpose {
            EmbeddedTransactionPurpose::Mutation => {
                Some(Arc::clone(&native_gate).acquire(operation).await?)
            }
            EmbeddedTransactionPurpose::ExecutionRenewal(_)
            | EmbeddedTransactionPurpose::ReadOnly => None,
        };
        let native_operations = match purpose {
            EmbeddedTransactionPurpose::ExecutionRenewal(native_operations) => native_operations,
            EmbeddedTransactionPurpose::Mutation | EmbeddedTransactionPurpose::ReadOnly => {
                Arc::new(NativeOperationTracker::default())
            }
        };
        let (rollback_on_drop, handle) = begin_embedded_owned(
            Arc::clone(node),
            native_gate,
            read_only,
            operation,
            write_guard,
            Arc::clone(&native_operations),
            cancellation_rollback_scheduled,
            |_| std::future::ready(()),
        )
        .await?;
        Ok(ConfigApplyTxn {
            backend: TxnBackend::Embedded {
                node: Arc::clone(node),
                handle: handle.clone(),
                identity,
                native_operations,
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
            contends_with_request_siblings: AtomicBool::new(false),
            operation: Some(operation.as_str()),
        })
    }

    #[cfg(test)]
    pub(crate) async fn begin_execution_renewal_for_test(
        node: &Arc<EmbeddedNode>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            None,
            WriteOperation::new("test.execution_renewal")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::ExecutionRenewal(Arc::new(
                NativeOperationTracker::default(),
            )),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_local(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            identity,
            WriteOperation::new("test.begin_local")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::Mutation,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn pause_drop_rollback_for_test(&mut self, pause: Arc<RollbackDropCleanupPause>) {
        let rollback = self
            .rollback_on_drop
            .as_mut()
            .expect("new transaction owns its rollback guard");
        let RollbackOnDrop::Embedded {
            drop_cleanup_pause, ..
        } = rollback
        else {
            panic!("test cleanup pause requires an embedded transaction");
        };
        *drop_cleanup_pause = Some(pause);
    }

    pub(super) async fn begin_http(
        endpoint: &'a GraphqlEndpoint,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        read_only: bool,
    ) -> Result<Self> {
        let (client, id, rollback_on_drop) = tokio::spawn(begin_http_owned(
            endpoint.clone(),
            cancellation_rollback_scheduled,
            read_only,
        ))
        .await
        .map_err(ConfigClientError::BeginTask)??;
        let txn = Self {
            backend: TxnBackend::Http {
                endpoint,
                id: id.clone(),
                client: client.clone(),
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
            contends_with_request_siblings: AtomicBool::new(false),
            operation: None,
        };
        Ok(txn)
    }

    pub(super) fn affected_documents(&self) -> u64 {
        self.affected_documents.load(Ordering::Relaxed)
    }

    /// Marks this attempt as writing a key that every concurrent sibling of
    /// its request also writes: the background admission sequence, or the
    /// session activity each published tool terminal refreshes. A conflict
    /// then retries within the sibling budget, since such siblings conflict
    /// until one commits.
    pub(crate) fn contend_with_request_siblings(&self) {
        self.contends_with_request_siblings
            .store(true, Ordering::Relaxed);
    }
}
