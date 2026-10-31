use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use defra_node::EmbeddedNode;
use futures::future::BoxFuture;
use query::TransactionHandle;
use tracing::{instrument::WithSubscriber, Instrument};

#[cfg(test)]
use super::fault_injection::mark_cleanup_scheduled_for_test;
use super::gate::{MutationWriteGate, MutationWriteGuard};
use super::native::NativeOperationTracker;
use super::{
    embedded_phase_timeout, graphql, retry, ConfigClientError, GraphqlEndpoint, Result,
    EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT,
};
use crate::config_client::write_telemetry::{record_slow_native_phase, WriteOperation};
use crate::drop_cleanup::DropCleanup;

pub(super) async fn cleanup_while_holding_write_gate<G, F, T>(
    write_guard: Option<G>,
    cleanup: F,
) -> std::result::Result<T, tokio::time::error::Elapsed>
where
    F: Future<Output = T>,
{
    let _write_guard = write_guard;
    tokio::time::timeout(EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT, cleanup).await
}

// Cleanup owners may drop on threads without a Tokio context, so each retains
// the runtime that created it.
pub(super) enum RollbackOnDrop {
    Http {
        runtime: DropCleanup,
        endpoint: GraphqlEndpoint,
        id: String,
        client: reqwest::Client,
        armed: bool,
    },
    Embedded {
        runtime: DropCleanup,
        node: Arc<EmbeddedNode>,
        native_gate: Arc<MutationWriteGate>,
        native_owner: StdMutex<Option<BoxFuture<'static, Result<()>>>>,
        write_guard: Option<MutationWriteGuard>,
        #[cfg(test)]
        drop_cleanup_pause: Option<Arc<RollbackDropCleanupPause>>,
        native_operations: Arc<NativeOperationTracker>,
        armed: bool,
    },
}

#[cfg(test)]
pub(crate) struct RollbackDropCleanupPause {
    pub(crate) started: Arc<tokio::sync::Notify>,
    pub(crate) release: Arc<tokio::sync::Notify>,
}

impl RollbackOnDrop {
    pub(super) fn disarm(&mut self) {
        match self {
            Self::Http { armed, .. } | Self::Embedded { armed, .. } => *armed = false,
        }
    }
}

impl Drop for RollbackOnDrop {
    fn drop(&mut self) {
        match self {
            Self::Http {
                runtime,
                endpoint,
                id,
                client,
                armed,
            } => {
                if !*armed {
                    return;
                }
                let endpoint = endpoint.clone();
                let id = id.clone();
                let client = client.clone();
                runtime.spawn(async move {
                    if let Err(error) = graphql::txn_discard(&endpoint, &id, &client).await {
                        tracing::debug!(%error, "discarding cancelled HTTP transaction");
                    }
                });
            }
            Self::Embedded {
                runtime,
                node,
                native_gate,
                native_owner,
                write_guard,
                #[cfg(test)]
                drop_cleanup_pause,
                native_operations,
                armed,
            } => {
                drop(
                    native_owner
                        .get_mut()
                        .unwrap_or_else(|e| e.into_inner())
                        .take(),
                );
                if !*armed {
                    return;
                }
                #[cfg(test)]
                let pause_cleanup = drop_cleanup_pause.is_some();
                #[cfg(not(test))]
                let pause_cleanup = false;
                let in_flight = native_operations
                    .state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .in_flight;
                if in_flight == 0 && !pause_cleanup {
                    return;
                }
                let node = Arc::clone(node);
                let native_gate = Arc::clone(native_gate);
                let native_operations = Arc::clone(native_operations);
                let write_guard = write_guard.take();
                #[cfg(test)]
                let drop_cleanup_pause = drop_cleanup_pause.take();
                let dispatcher = tracing::dispatcher::get_default(Clone::clone);
                let span = tracing::Span::current();
                runtime.spawn(async move {
                    let _native_gate = native_gate;
                    native_operations.wait_idle().await;
                    let cleanup = async move {
                        #[cfg(test)]
                        if let Some(pause) = drop_cleanup_pause {
                            pause.started.notify_one();
                            pause.release.notified().await;
                        }
                        drop(node);
                        Ok::<(), ConfigClientError>(())
                    };
                    match cleanup_while_holding_write_gate(
                        write_guard,
                        cleanup,
                    )
                    .await {
                        Ok(Ok(())) => {}
                        Ok(Err(error)) => {
                            tracing::debug!(%error, "releasing cancelled embedded transaction ownership");
                        }
                        Err(_) => {
                            tracing::warn!(
                                timeout_ms = EMBEDDED_TRANSACTION_ROLLBACK_TIMEOUT.as_millis(),
                                "cancelled embedded transaction gate-release pause timed out"
                            );
                        }
                    }
                }
                .with_subscriber(dispatcher)
                .instrument(span));
                #[cfg(test)]
                mark_cleanup_scheduled_for_test();
            }
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "ownership handles moved into the transaction owner task"
)]
pub(super) async fn begin_embedded_owned<F, Fut>(
    node: Arc<EmbeddedNode>,
    native_gate: Arc<MutationWriteGate>,
    read_only: bool,
    operation: WriteOperation,
    write_guard: Option<MutationWriteGuard>,
    native_operations: Arc<NativeOperationTracker>,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
    after_begin: F,
) -> Result<(RollbackOnDrop, TransactionHandle)>
where
    F: FnOnce(TransactionHandle) -> Fut,
    Fut: Future<Output = ()>,
{
    let (registered, registration) = tokio::sync::oneshot::channel();
    let owner_node = Arc::clone(&node);
    let mut native_owner: BoxFuture<'static, Result<()>> = Box::pin(async move {
        let transaction = owner_node
            .begin_transaction_guard(read_only)
            .await
            .map_err(|error| {
                retry::transaction_storage_failure(ConfigClientError::BeginTransaction {
                    source: error,
                })
            })?;
        let _ = registered.send(transaction.handle().clone());
        std::future::pending::<()>().await;
        drop(transaction);
        Ok(())
    });
    let started = Instant::now();
    let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, async {
        tokio::select! {
            result = &mut native_owner => {
                result?;
                Err(ConfigClientError::OwnerEndedBeforeRegistration)
            }
            result = registration => result.map_err(ConfigClientError::Registration),
        }
    })
    .await;
    record_slow_native_phase(Some(operation.as_str()), "begin", started.elapsed());
    let handle = result.map_err(|_| {
        embedded_phase_timeout("begin", EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT)
    })??;
    let rollback = RollbackOnDrop::Embedded {
        runtime: DropCleanup::current(),
        node: Arc::clone(&node),
        native_gate,
        native_owner: StdMutex::new(Some(native_owner)),
        write_guard,
        #[cfg(test)]
        drop_cleanup_pause: None,
        native_operations,
        armed: true,
    };
    let _ = cancellation_rollback_scheduled;
    after_begin(handle.clone()).await;
    Ok((rollback, handle))
}

pub(super) async fn begin_http_owned(
    endpoint: GraphqlEndpoint,
    cancellation_rollback_scheduled: Arc<AtomicBool>,
    read_only: bool,
) -> Result<(reqwest::Client, String, RollbackOnDrop)> {
    let (client, id) = graphql::txn_begin(&endpoint, read_only)
        .await
        .map_err(retry::transaction_storage_failure)?;
    let rollback = RollbackOnDrop::Http {
        runtime: DropCleanup::current(),
        endpoint: endpoint.clone(),
        id: id.clone(),
        client: client.clone(),
        armed: true,
    };
    cancellation_rollback_scheduled.store(true, Ordering::Release);
    Ok((client, id, rollback))
}
