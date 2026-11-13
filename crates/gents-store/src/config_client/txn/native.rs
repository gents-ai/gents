use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Instant;

use tracing::{instrument::WithSubscriber, Instrument};

#[cfg(any(test, feature = "test-fixtures"))]
use super::fault_injection::SUCCESSFUL_MUTATION_FAULT;
use super::{retry, ConfigClientError, Result, EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT};
use crate::config_client::write_telemetry::{record_slow_native_phase, WriteOperation};

tokio::task_local! {
    /// The operation whose embedded transaction callback is running on this
    /// task. Not request context: the callback is handed its transaction
    /// explicitly, and this exists to refuse a canonical write that ignores
    /// that handle, which would wait on the single embedded writer its own
    /// transaction holds. Such a write has by definition not been passed
    /// anything, so only the task can tell it where it is.
    pub(super) static ACTIVE_EMBEDDED_TRANSACTION: &'static str;
}

#[derive(Default)]
pub(super) struct NativeOperationState {
    pub(super) in_flight: usize,
    abandoned: bool,
}

#[derive(Default)]
pub(super) struct NativeOperationTracker {
    pub(super) state: StdMutex<NativeOperationState>,
    idle: tokio::sync::Notify,
    abandoned: tokio::sync::Notify,
}

impl NativeOperationTracker {
    pub(super) fn start(self: &Arc<Self>) -> Result<NativeOperationPermit> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.abandoned {
            return Err(native_operation_abandoned(
                ConfigClientError::PoisonedByAbandonedOperation,
            ));
        }
        state.in_flight += 1;
        drop(state);
        Ok(NativeOperationPermit(Arc::clone(self)))
    }

    fn abandon(&self) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abandoned = true;
        self.abandoned.notify_waiters();
    }

    pub(super) fn is_abandoned(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abandoned
    }

    /// The abandonment counterpart of `wait_idle`: resolves once the caller
    /// guard has poisoned this registry. `notify_waiters` keeps no permit, so
    /// the waiter registers before it reads the flag and an abandonment that
    /// lands first is still observed.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(super) async fn wait_abandoned(&self) {
        loop {
            let notified = self.abandoned.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_abandoned() {
                return;
            }
            notified.await;
        }
    }

    pub(super) async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let idle = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .in_flight
                == 0;
            if idle {
                return;
            }
            notified.await;
        }
    }
}

pub(super) struct NativeOperationPermit(Arc<NativeOperationTracker>);

impl Drop for NativeOperationPermit {
    fn drop(&mut self) {
        let became_idle = {
            let mut state = self
                .0
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.in_flight -= 1;
            state.in_flight == 0
        };
        if became_idle {
            self.0.idle.notify_waiters();
        }
    }
}

struct CallerOperationGuard {
    tracker: Arc<NativeOperationTracker>,
    completed: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("native operation remains owned after caller abandonment")]
pub struct NativeOperationAbandoned;

pub(super) fn native_operation_abandoned(
    phase: fn(NativeOperationAbandoned) -> ConfigClientError,
) -> ConfigClientError {
    retry::transaction_storage_failure(phase(NativeOperationAbandoned))
}

impl CallerOperationGuard {
    fn new(tracker: Arc<NativeOperationTracker>) -> Self {
        Self {
            tracker,
            completed: false,
        }
    }

    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for CallerOperationGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.tracker.abandon();
        }
    }
}

pub(super) async fn execute_owned_native<T, F>(
    native: F,
    tracker: Arc<NativeOperationTracker>,
    operation: Option<&'static str>,
) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = T> + Send + 'static,
{
    let permit = tracker.start()?;
    let mut caller = CallerOperationGuard::new(Arc::clone(&tracker));
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    let span = tracing::Span::current();
    let (send, receive) = tokio::sync::oneshot::channel();
    #[cfg(any(test, feature = "test-fixtures"))]
    let native_pause = SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| fault.native_pause.clone())
        .ok()
        .flatten();
    #[cfg(any(test, feature = "test-fixtures"))]
    if let Some(observation) = native_pause
        .as_ref()
        .and_then(|pause| pause.observation.as_ref())
    {
        observation.publish(&tracker);
    }
    let dispatched_at = Instant::now();
    tokio::spawn(
        async move {
            record_slow_native_phase(operation, "execute-dispatch", dispatched_at.elapsed());
            #[cfg(any(test, feature = "test-fixtures"))]
            if let Some(pause) = native_pause {
                pause.reached.notify_one();
                pause.release.notified().await;
            }
            let native_started = Instant::now();
            let result = match operation {
                Some(operation) => ACTIVE_EMBEDDED_TRANSACTION.scope(operation, native).await,
                None => native.await,
            };
            record_slow_native_phase(operation, "execute-native-await", native_started.elapsed());
            drop(permit);
            let _ = send.send(result);
        }
        .with_subscriber(dispatcher)
        .instrument(span),
    );
    let result = tokio::time::timeout(EMBEDDED_TRANSACTION_STORAGE_STEP_TIMEOUT, receive)
        .await
        .map_err(|_| native_operation_abandoned(ConfigClientError::ExecuteCallerTimeout))?
        .map_err(|_| native_operation_abandoned(ConfigClientError::ExecutionTaskEnded))?;
    caller.complete();
    Ok(result)
}

#[derive(Debug, thiserror::Error)]
#[error(
    "embedded canonical write {waiting_operation} is nested inside transaction {owner_operation}; use the supplied transaction owner"
)]
pub struct ReentrantEmbeddedWrite {
    waiting_operation: &'static str,
    owner_operation: &'static str,
}

pub(super) fn ensure_not_reentrant_embedded_write(operation: WriteOperation) -> Result<()> {
    if let Ok(owner_operation) = ACTIVE_EMBEDDED_TRANSACTION.try_with(|owner| *owner) {
        tracing::error!(
            waiting_operation = operation.as_str(),
            owner_operation,
            "re-entrant embedded canonical write rejected"
        );
        return Err(ReentrantEmbeddedWrite {
            waiting_operation: operation.as_str(),
            owner_operation,
        }
        .into());
    }
    Ok(())
}
