//! Asynchronous cleanup owed by a guard's synchronous `Drop`.

use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// The runtime a guard was created on, to which its synchronous `Drop` hands
/// asynchronous cleanup.
///
/// `Drop` may run on a thread without a Tokio context (an unwinding or aborted
/// task, a runtime shutting down), so a guard binds its runtime when it is
/// created. Cleanup handed over after that runtime shut down is dropped
/// without running.
#[derive(Clone, Debug)]
pub struct DropCleanup {
    runtime: tokio::runtime::Handle,
    tracker: Option<CleanupTracker>,
}

impl DropCleanup {
    /// Binds the runtime of the calling task; panics outside a Tokio runtime.
    pub fn current() -> Self {
        Self {
            runtime: current_handle(),
            tracker: None,
        }
    }

    /// Binds the calling task's runtime and counts the cleanup it spawns in
    /// `tracker`, so that tracker's owner can flush it.
    pub fn tracked(tracker: &CleanupTracker) -> Self {
        Self {
            runtime: current_handle(),
            tracker: Some(tracker.clone()),
        }
    }

    pub fn spawn<F>(&self, cleanup: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Some(tracker) = self.tracker.as_ref() else {
            drop(self.runtime.spawn(cleanup));
            return;
        };
        let in_flight = tracker.enter();
        drop(self.runtime.spawn(async move {
            let _in_flight = in_flight;
            cleanup.await;
        }));
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "a drop guard binds the runtime of the task that creates it, because its Drop may run where no runtime is current"
)]
fn current_handle() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

/// Counts the cleanup a set of guards has handed to the runtime and not yet
/// finished.
///
/// Cleanup spawned from `Drop` is detached, so a runtime shutting down while it
/// is in flight drops it unrun and a permit dropped at that moment loses the
/// terminal row its `Drop` owed. [`CleanupTracker::idle`] is the flush point an
/// owner awaits before releasing the runtime it handed that work to.
#[derive(Clone, Debug, Default)]
pub struct CleanupTracker(Arc<TrackerState>);

#[derive(Debug, Default)]
struct TrackerState {
    in_flight: AtomicUsize,
    idle: tokio::sync::Notify,
}

impl CleanupTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolves once no cleanup counted by this tracker is in flight.
    pub async fn idle(&self) {
        loop {
            let notified = self.0.idle.notified();
            tokio::pin!(notified);
            // Register before reading the count: a cleanup finishing between
            // the read and the wait would otherwise notify nobody.
            notified.as_mut().enable();
            if self.0.in_flight.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn enter(&self) -> InFlight {
        self.0.in_flight.fetch_add(1, Ordering::SeqCst);
        InFlight(self.0.clone())
    }
}

/// Counts one cleanup down on completion, and equally when the runtime drops it
/// unrun, so `idle` cannot outlive the runtime it waits on.
struct InFlight(Arc<TrackerState>);

impl Drop for InFlight {
    fn drop(&mut self) {
        if self.0.in_flight.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}
