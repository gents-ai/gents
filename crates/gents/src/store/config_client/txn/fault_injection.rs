use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use super::{ConfigApplyTxn, ConfigClientError, Result};

pub(super) struct SuccessfulMutationFault {
    fail_after: Option<usize>,
    count: AtomicUsize,
    lose_receipt: bool,
    receipt_operation: Option<&'static str>,
    receipt_fired: AtomicBool,
    pause: Option<SuccessfulMutationPause>,
    pub(super) write_gate_observation: Option<Arc<WriteGateObservation>>,
    pub(super) read_storage_failure: Option<&'static str>,
    pub(super) read_pause: Option<(&'static str, Arc<tokio::sync::Notify>)>,
    pub(super) native_pause: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    fail_rollback: bool,
    retry_backoff_reached: Option<Arc<tokio::sync::Notify>>,
    cleanup_scheduled: Option<Arc<AtomicBool>>,
}

pub(super) struct LocalOperationPause {
    pub(super) reached: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
    pub(super) completed: Option<Arc<tokio::sync::Notify>>,
}

tokio::task_local! {
    pub(super) static LOCAL_OPERATION_PAUSE: Arc<LocalOperationPause>;
}

struct SuccessfulMutationPause {
    after: usize,
    reached: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    fired: AtomicBool,
}

pub(super) struct WriteGateObservation {
    pub(super) queued: Arc<tokio::sync::Notify>,
    pub(super) acquired: Arc<AtomicBool>,
    pub(super) queued_fired: AtomicBool,
}

tokio::task_local! {
    pub(super) static SUCCESSFUL_MUTATION_FAULT: Arc<SuccessfulMutationFault>;
}

pub(super) async fn after_successful_mutation_for_test() -> Result<()> {
    let Ok(fault) = SUCCESSFUL_MUTATION_FAULT.try_with(Arc::clone) else {
        return Ok(());
    };
    let count = fault.count.fetch_add(1, Ordering::Relaxed) + 1;
    if fault.fail_after == Some(count) {
        return Err(ConfigClientError::InjectedMutationFailure { count });
    }
    if let Some(pause) = fault.pause.as_ref() {
        if pause.after == count && !pause.fired.swap(true, Ordering::Relaxed) {
            pause.reached.notify_one();
            pause.release.notified().await;
        }
    }
    Ok(())
}

pub(super) fn fail_rollback_for_test() -> bool {
    SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| fault.fail_rollback)
        .unwrap_or(false)
}

pub(super) fn retry_backoff_reached_for_test() {
    let _ = SUCCESSFUL_MUTATION_FAULT.try_with(|fault| {
        if let Some(reached) = fault.retry_backoff_reached.as_ref() {
            reached.notify_one();
        }
    });
}

pub(super) fn mark_cleanup_scheduled_for_test() {
    let _ = SUCCESSFUL_MUTATION_FAULT.try_with(|fault| {
        if let Some(scheduled) = fault.cleanup_scheduled.as_ref() {
            scheduled.store(true, Ordering::Release);
        }
    });
}

pub(super) fn post_commit_receipt_loss_for_test(operation: Option<&str>) -> bool {
    SUCCESSFUL_MUTATION_FAULT
        .try_with(|fault| {
            if !fault.lose_receipt
                || fault
                    .receipt_operation
                    .is_some_and(|target| Some(target) != operation)
            {
                return false;
            }
            !fault.receipt_fired.swap(true, Ordering::Relaxed)
        })
        .unwrap_or(false)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EmbeddedFinalizationBoundary {
    BeforeCommit,
    AfterCommit,
    BeforeRollback,
}

tokio::task_local! {
    pub(super) static EMBEDDED_FINALIZATION_PAUSE: (
        EmbeddedFinalizationBoundary,
        Arc<tokio::sync::Notify>,
    );
}

pub(super) async fn pause_embedded_finalization_for_test(boundary: EmbeddedFinalizationBoundary) {
    let reached = EMBEDDED_FINALIZATION_PAUSE
        .try_with(|(selected, reached)| (*selected == boundary).then(|| Arc::clone(reached)))
        .ok()
        .flatten();
    if let Some(reached) = reached {
        reached.notify_one();
        std::future::pending::<()>().await;
    }
}

impl ConfigApplyTxn<'_> {
    pub(crate) async fn with_embedded_commit_ack_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        EMBEDDED_FINALIZATION_PAUSE
            .scope((EmbeddedFinalizationBoundary::AfterCommit, reached), future)
            .await
    }

    pub(crate) async fn with_owned_local_operation_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        LOCAL_OPERATION_PAUSE
            .scope(
                Arc::new(LocalOperationPause {
                    reached,
                    release,
                    completed: None,
                }),
                future,
            )
            .await
    }

    pub(crate) async fn with_owned_local_operation_completion<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        completed: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        LOCAL_OPERATION_PAUSE
            .scope(
                Arc::new(LocalOperationPause {
                    reached,
                    release,
                    completed: Some(completed),
                }),
                future,
            )
            .await
    }

    pub(crate) async fn with_successful_mutation_failure_at<F>(
        fail_after: Option<usize>,
        future: F,
    ) -> (F::Output, usize)
    where
        F: Future,
    {
        assert!(fail_after.is_none_or(|index| index > 0));
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (output, fault.count.load(Ordering::Relaxed))
    }

    pub(crate) async fn assert_every_mutation_rolls_back<T, E, S, A, AF, O, OF>(
        mut attempt: A,
        observe: O,
    ) -> (T, usize)
    where
        E: std::fmt::Debug,
        S: PartialEq + std::fmt::Debug,
        A: FnMut() -> AF,
        AF: Future<Output = std::result::Result<T, E>>,
        O: Fn() -> OF,
        OF: Future<Output = S>,
    {
        let before = observe().await;
        let mut index = 1;
        loop {
            let (result, observed) =
                Self::with_successful_mutation_failure_at(Some(index), attempt()).await;
            match result {
                Ok(value) => {
                    assert_eq!(
                        observed,
                        index - 1,
                        "a committed attempt must not pass the injected position"
                    );
                    assert_ne!(
                        observe().await,
                        before,
                        "the committed attempt must be visible to the rollback observation"
                    );
                    return (value, observed);
                }
                Err(error) => {
                    let detail = format!("{error:?}");
                    assert!(
                        detail.contains(&format!(
                            "injected failure after successful transaction mutation {index}"
                        )) || detail
                            .contains(&format!("InjectedMutationFailure {{ count: {index} }}")),
                        "unexpected failure at mutation {index}: {detail}"
                    );
                    assert_eq!(observed, index, "injection must fire exactly once");
                    assert_eq!(
                        observe().await,
                        before,
                        "mutation {index} leaked a partial write"
                    );
                }
            }
            index += 1;
        }
    }

    pub(crate) async fn with_post_commit_receipt_loss<F>(future: F) -> (F::Output, bool)
    where
        F: Future,
    {
        Self::with_post_commit_receipt_loss_for_operation(None, future).await
    }

    pub(crate) async fn with_post_commit_receipt_loss_for_operation<F>(
        operation: Option<&'static str>,
        future: F,
    ) -> (F::Output, bool)
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: true,
            receipt_operation: operation,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (output, fault.receipt_fired.load(Ordering::Relaxed))
    }

    pub(crate) async fn with_successful_mutation_pause_at<F>(
        after: usize,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, bool)
    where
        F: Future,
    {
        assert!(after > 0);
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: Some(SuccessfulMutationPause {
                after,
                reached,
                release,
                fired: AtomicBool::new(false),
            }),
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        let output = SUCCESSFUL_MUTATION_FAULT
            .scope(Arc::clone(&fault), future)
            .await;
        (
            output,
            fault
                .pause
                .as_ref()
                .expect("pause configured")
                .fired
                .load(Ordering::Relaxed),
        )
    }

    pub(crate) async fn with_write_gate_observation<F>(
        queued: Arc<tokio::sync::Notify>,
        acquired: Arc<AtomicBool>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: Some(Arc::new(WriteGateObservation {
                queued,
                acquired,
                queued_fired: AtomicBool::new(false),
            })),
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub(crate) async fn with_read_storage_failure<F>(marker: &'static str, future: F) -> F::Output
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: Some(marker),
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub(crate) async fn with_read_pause<F>(
        marker: &'static str,
        reached: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: Some((marker, reached)),
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub(crate) async fn with_native_operation_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: Some((reached, release)),
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub(crate) async fn with_rollback_failure_retry_boundary<F>(
        reached: Arc<tokio::sync::Notify>,
        cleanup_scheduled: Arc<AtomicBool>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        let fault = Arc::new(SuccessfulMutationFault {
            fail_after: None,
            count: AtomicUsize::new(0),
            lose_receipt: false,
            receipt_operation: None,
            receipt_fired: AtomicBool::new(false),
            pause: None,
            write_gate_observation: None,
            read_storage_failure: None,
            read_pause: None,
            native_pause: None,
            fail_rollback: true,
            retry_backoff_reached: Some(reached),
            cleanup_scheduled: Some(cleanup_scheduled),
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }
}
