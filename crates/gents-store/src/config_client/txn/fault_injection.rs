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
    pub(super) read_storage_failure: Option<&'static str>,
    pub(super) read_pause: Option<(&'static str, Arc<tokio::sync::Notify>)>,
    pub(super) native_pause: Option<Arc<NativeOperationPause>>,
    fail_rollback: bool,
    retry_backoff_reached: Option<Arc<tokio::sync::Notify>>,
    cleanup_scheduled: Option<Arc<AtomicBool>>,
}

pub(super) struct NativeOperationPause {
    pub(super) reached: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
    pub(super) observation: Option<Arc<NativeOperationObservation>>,
}

/// The transaction owner's native operation registry, handed to a driver that
/// cannot reach it otherwise: the execution-renewal owner creates the registry
/// for itself, so a caller of that owner never holds the tracker whose state
/// the model's observations describe.
#[derive(Default)]
pub struct NativeOperationObservation {
    tracker: std::sync::Mutex<Option<Arc<super::NativeOperationTracker>>>,
    published: tokio::sync::Notify,
}

impl NativeOperationObservation {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Arc<super::NativeOperationTracker>>> {
        self.tracker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Keeps the first registry a scope admits, so a renewal that runs again
    /// after the observed attempt drains cannot replace it.
    pub(super) fn publish(&self, tracker: &Arc<super::NativeOperationTracker>) {
        let mut slot = self.slot();
        if slot.is_none() {
            *slot = Some(Arc::clone(tracker));
            drop(slot);
            self.published.notify_waiters();
        }
    }

    fn tracker(&self) -> Option<Arc<super::NativeOperationTracker>> {
        self.slot().clone()
    }

    /// Resolves once the owner has abandoned the registry, the state the Lean
    /// model observes as `callerAbandonmentObserved`. Both waits register
    /// before they read, so neither publication nor abandonment is lost by
    /// landing first.
    pub async fn abandoned(&self) {
        let tracker = loop {
            let notified = self.published.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(tracker) = self.tracker() {
                break tracker;
            }
            notified.await;
        };
        tracker.wait_abandoned().await;
    }

    pub fn is_abandoned(&self) -> bool {
        self.tracker().is_some_and(|tracker| tracker.is_abandoned())
    }

    pub fn in_flight(&self) -> usize {
        self.tracker().map_or(0, |tracker| {
            tracker
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .in_flight
        })
    }
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
    every: bool,
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
    pub(super) static WRITE_GATE_OBSERVATION: Arc<WriteGateObservation>;
    pub(super) static SUCCESSFUL_MUTATION_FAULT: Arc<SuccessfulMutationFault>;
}

tokio::task_local! {
    static SUCCESSFUL_MUTATION_TRACE: Arc<std::sync::Mutex<Vec<Option<&'static str>>>>;
}

pub(super) async fn after_successful_mutation_for_test(
    operation: Option<&'static str>,
) -> Result<()> {
    let Ok(fault) = SUCCESSFUL_MUTATION_FAULT.try_with(Arc::clone) else {
        return Ok(());
    };
    let _ = SUCCESSFUL_MUTATION_TRACE.try_with(|trace| {
        trace
            .lock()
            .expect("mutation trace lock is never poisoned")
            .push(operation)
    });
    let count = fault.count.fetch_add(1, Ordering::Relaxed) + 1;
    if fault.fail_after == Some(count) {
        return Err(ConfigClientError::InjectedMutationFailure { count });
    }
    if let Some(pause) = fault.pause.as_ref() {
        let due = if pause.every {
            count >= pause.after
        } else {
            pause.after == count
        };
        if due && (!pause.fired.swap(true, Ordering::Relaxed) || pause.every) {
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
    AfterRollback,
    BeforeRollbackTimeoutRelease,
}

#[derive(Clone)]
pub(super) struct BoundaryPause {
    pub(super) reached: Arc<tokio::sync::Notify>,
    pub(super) release: Arc<tokio::sync::Notify>,
}

#[derive(Default)]
pub(super) struct EmbeddedFinalizationLifecycle {
    pub(super) before_commit: Option<BoundaryPause>,
    pub(super) after_commit: Option<BoundaryPause>,
    pub(super) before_rollback: Option<BoundaryPause>,
    pub(super) after_rollback: Option<BoundaryPause>,
    pub(super) before_rollback_timeout_release: Option<BoundaryPause>,
    pub(super) on_rollback_drop: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl EmbeddedFinalizationLifecycle {
    fn pause_for(&self, boundary: EmbeddedFinalizationBoundary) -> Option<BoundaryPause> {
        match boundary {
            EmbeddedFinalizationBoundary::BeforeCommit => self.before_commit.clone(),
            EmbeddedFinalizationBoundary::AfterCommit => self.after_commit.clone(),
            EmbeddedFinalizationBoundary::BeforeRollback => self.before_rollback.clone(),
            EmbeddedFinalizationBoundary::AfterRollback => self.after_rollback.clone(),
            EmbeddedFinalizationBoundary::BeforeRollbackTimeoutRelease => {
                self.before_rollback_timeout_release.clone()
            }
        }
    }
}

pub(super) struct EmbeddedRollbackDropObserver {
    on_drop: Option<Arc<dyn Fn() + Send + Sync>>,
    disarmed: bool,
}

impl EmbeddedRollbackDropObserver {
    pub(super) fn for_test() -> Self {
        let on_drop = EMBEDDED_FINALIZATION_LIFECYCLE
            .try_with(|lifecycle| lifecycle.on_rollback_drop.clone())
            .ok()
            .flatten();
        Self {
            on_drop,
            disarmed: false,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.disarmed = true;
    }
}

impl Drop for EmbeddedRollbackDropObserver {
    fn drop(&mut self) {
        if !self.disarmed {
            if let Some(on_drop) = &self.on_drop {
                on_drop();
            }
        }
    }
}

tokio::task_local! {
    pub(super) static EMBEDDED_FINALIZATION_LIFECYCLE: Arc<EmbeddedFinalizationLifecycle>;
    pub(super) static EMBEDDED_FINALIZATION_PAUSE: (
        EmbeddedFinalizationBoundary,
        Arc<tokio::sync::Notify>,
    );
    static EMBEDDED_FINALIZATION_RELEASE: (Arc<tokio::sync::Notify>, Arc<AtomicBool>);
}

pub(super) async fn pause_embedded_finalization_for_test(boundary: EmbeddedFinalizationBoundary) {
    if let Ok(Some(pause)) =
        EMBEDDED_FINALIZATION_LIFECYCLE.try_with(|lifecycle| lifecycle.pause_for(boundary))
    {
        pause.reached.notify_one();
        pause.release.notified().await;
        return;
    }
    let reached = EMBEDDED_FINALIZATION_PAUSE
        .try_with(|(selected, reached)| (*selected == boundary).then(|| Arc::clone(reached)))
        .ok()
        .flatten();
    if let Some(reached) = reached {
        let release = EMBEDDED_FINALIZATION_RELEASE
            .try_with(|(release, fired)| (Arc::clone(release), Arc::clone(fired)))
            .ok();
        if release
            .as_ref()
            .is_some_and(|(_, fired)| fired.swap(true, Ordering::Relaxed))
        {
            return;
        }
        reached.notify_one();
        if let Some((release, _)) = release {
            release.notified().await;
        } else {
            std::future::pending::<()>().await;
        }
    }
}

impl ConfigApplyTxn<'_> {
    pub async fn with_embedded_before_commit_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        EMBEDDED_FINALIZATION_PAUSE
            .scope(
                (EmbeddedFinalizationBoundary::BeforeCommit, reached),
                EMBEDDED_FINALIZATION_RELEASE
                    .scope((release, Arc::new(AtomicBool::new(false))), future),
            )
            .await
    }

    /// The commit never reaches storage and the discard receipt arrives after
    /// its deadline, so neither operation confirms the outcome to the caller.
    pub async fn with_unlanded_commit_timeout<F>(
        committing: Arc<tokio::sync::Notify>,
        discarded: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        let held = |reached| {
            Some(BoundaryPause {
                reached,
                release: Default::default(),
            })
        };
        EMBEDDED_FINALIZATION_LIFECYCLE
            .scope(
                Arc::new(EmbeddedFinalizationLifecycle {
                    before_commit: held(committing),
                    after_rollback: held(discarded),
                    ..Default::default()
                }),
                future,
            )
            .await
    }

    pub async fn with_embedded_commit_ack_pause<F>(
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

    pub async fn with_owned_local_operation_pause<F>(
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

    pub async fn with_owned_local_operation_completion<F>(
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

    /// Like [`Self::with_successful_mutation_failure_at`], also returning the
    /// transaction operation of each successful mutation in order, so a test
    /// can target a mutation by the owner that issued it.
    pub async fn with_successful_mutation_failure_and_trace<F>(
        fail_after: Option<usize>,
        future: F,
    ) -> (F::Output, Vec<Option<&'static str>>)
    where
        F: Future,
    {
        let trace = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (output, _) = SUCCESSFUL_MUTATION_TRACE
            .scope(
                Arc::clone(&trace),
                Self::with_successful_mutation_failure_at(fail_after, future),
            )
            .await;
        let trace = trace
            .lock()
            .expect("mutation trace lock is never poisoned")
            .clone();
        (output, trace)
    }

    pub async fn with_successful_mutation_failure_at<F>(
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

    pub async fn assert_every_mutation_rolls_back<T, E, S, A, AF, O, OF>(
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

    pub async fn with_post_commit_receipt_loss<F>(future: F) -> (F::Output, bool)
    where
        F: Future,
    {
        Self::with_post_commit_receipt_loss_for_operation(None, future).await
    }

    pub async fn with_post_commit_receipt_loss_for_operation<F>(
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

    pub async fn with_successful_mutation_pause_at<F>(
        after: usize,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, bool)
    where
        F: Future,
    {
        let (output, fired, _) =
            Self::with_successful_mutation_pause_and_count(after, reached, release, future).await;
        (output, fired)
    }

    /// Runs `write` after the first successful mutation of `future`'s
    /// transactions and before that transaction commits, so `write` commits
    /// first and the paused commit meets DefraDB's own conflict when `write`
    /// touched a document the transaction read. Returns whether the pause was
    /// reached; `write` never runs if `future` settles without mutating.
    pub async fn with_concurrent_write_before_commit<F, W>(future: F, write: W) -> (F::Output, bool)
    where
        F: Future,
        W: Future<Output = ()>,
    {
        let reached = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let paused = std::pin::pin!(Self::with_successful_mutation_pause_at(
            1,
            reached.clone(),
            release.clone(),
            future,
        ));
        let driver = std::pin::pin!(async move {
            reached.notified().await;
            write.await;
            release.notify_one();
        });
        let (mut paused, mut driver) = (paused, driver);
        let mut driven = false;
        loop {
            tokio::select! {
                output = &mut paused => return output,
                () = &mut driver, if !driven => driven = true,
            }
        }
    }

    pub async fn with_successful_mutation_pause_and_count<F>(
        after: usize,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, bool, usize)
    where
        F: Future,
    {
        Self::with_successful_mutation_pauses(after, false, reached, release, future).await
    }

    /// Pauses after every successful mutation of `future`'s transactions, every
    /// attempt included, until each pause is released.
    pub async fn with_each_successful_mutation_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, usize)
    where
        F: Future,
    {
        let (output, _, count) =
            Self::with_successful_mutation_pauses(1, true, reached, release, future).await;
        (output, count)
    }

    async fn with_successful_mutation_pauses<F>(
        after: usize,
        every: bool,
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> (F::Output, bool, usize)
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
                every,
                reached,
                release,
                fired: AtomicBool::new(false),
            }),
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
            fault.count.load(Ordering::Relaxed),
        )
    }

    pub async fn with_write_gate_observation<F>(
        queued: Arc<tokio::sync::Notify>,
        acquired: Arc<AtomicBool>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        WRITE_GATE_OBSERVATION
            .scope(
                Arc::new(WriteGateObservation {
                    queued,
                    acquired,
                    queued_fired: AtomicBool::new(false),
                }),
                future,
            )
            .await
    }

    pub async fn with_read_storage_failure<F>(marker: &'static str, future: F) -> F::Output
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
            read_storage_failure: Some(marker),
            read_pause: None,
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub async fn with_read_pause<F>(
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
            read_storage_failure: None,
            read_pause: Some((marker, reached)),
            native_pause: None,
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub async fn with_native_operation_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        Self::with_paused_native_operation(reached, release, None, future).await
    }

    /// The pause plus the registry the owner admits under it, for a driver
    /// whose owner creates that registry internally.
    pub async fn with_observed_native_operation_pause<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        observation: Arc<NativeOperationObservation>,
        future: F,
    ) -> F::Output
    where
        F: Future,
    {
        Self::with_paused_native_operation(reached, release, Some(observation), future).await
    }

    async fn with_paused_native_operation<F>(
        reached: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        observation: Option<Arc<NativeOperationObservation>>,
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
            read_storage_failure: None,
            read_pause: None,
            native_pause: Some(Arc::new(NativeOperationPause {
                reached,
                release,
                observation,
            })),
            fail_rollback: false,
            retry_backoff_reached: None,
            cleanup_scheduled: None,
        });
        SUCCESSFUL_MUTATION_FAULT.scope(fault, future).await
    }

    pub async fn with_rollback_failure_retry_boundary<F>(
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

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StatementCounts {
    statements: Vec<(Option<&'static str>, String)>,
}

impl StatementCounts {
    pub fn total(&self) -> usize {
        self.statements.len()
    }

    pub fn mutations(&self) -> usize {
        self.statements
            .iter()
            .filter(|(_, document)| document.trim_start().starts_with("mutation"))
            .count()
    }

    pub fn for_operation(&self, operation: &str) -> usize {
        self.statements
            .iter()
            .filter(|(label, _)| *label == Some(operation))
            .count()
    }

    pub fn transactions(&self, operation: &str) -> usize {
        let mut count = 0;
        let mut inside = false;
        for (label, _) in &self.statements {
            let matches = *label == Some(operation);
            if matches && !inside {
                count += 1;
            }
            inside = matches;
        }
        count
    }

    pub fn statements(&self) -> impl Iterator<Item = (Option<&'static str>, &str)> {
        self.statements
            .iter()
            .map(|(label, document)| (*label, document.as_str()))
    }
}

tokio::task_local! {
    static STATEMENT_LOG: Arc<std::sync::Mutex<Vec<(Option<&'static str>, String)>>>;
}

pub(in crate::config_client) fn record_statement(operation: Option<&'static str>, document: &str) {
    let _ = STATEMENT_LOG.try_with(|log| {
        log.lock()
            .expect("statement log lock is never poisoned")
            .push((operation, document.to_owned()))
    });
}

pub async fn count_statements<F>(future: F) -> (F::Output, StatementCounts)
where
    F: Future,
{
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output = STATEMENT_LOG.scope(log.clone(), future).await;
    let statements = log
        .lock()
        .expect("statement log lock is never poisoned")
        .clone();
    (output, StatementCounts { statements })
}
