//! How many snapshots a database still pins, and the wait for the last of
//! them to be released.
//!
//! [`Db::wait_for_snapshots`](crate::Db::wait_for_snapshots) never waits: it
//! returns a [`SnapshotWait`] at once. The handle reports the pins live now,
//! is a future a task awaits, and blocks the calling thread only when asked
//! to by name ([`SnapshotWait::wait`]).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::engine::RegolithEngine;

/// The snapshots a database pins now, and a wait for the last of them to be
/// released. Returned by [`Db::wait_for_snapshots`](crate::Db::wait_for_snapshots).
///
/// It counts pins, not handles: an iterator taken from a
/// [`Snapshot`](crate::Snapshot) pins it again for as long as the iterator
/// lives, and an open transaction pins its begin snapshot.
///
/// Three ways to use it, none of which the call that made it chose for the
/// caller:
///
/// - **Ask.** [`pinned`](Self::pinned) and [`is_ready`](Self::is_ready) read
///   the pins live at that moment and never wait.
/// - **Await.** It is a [`Future`] with output `()`, ready once no snapshot is
///   pinned. It needs no runtime and reads no clock: the release of the last
///   pin wakes the task, on the thread that released it. Ready says nothing
///   was pinned at that moment; a snapshot taken after that is not waited
///   for, and a wait polled again starts over. Dropping a pending wait
///   withdraws it.
/// - **Block.** [`wait`](Self::wait) parks the calling thread until no
///   snapshot is pinned or a timeout passes.
///
/// A database closed with snapshots live is consistent, just not tidy, so
/// what to do about outstanding pins is the caller's decision.
pub struct SnapshotWait {
    engine: Arc<RegolithEngine>,
    /// The wait a task polls, made at its first pending poll and let go once
    /// it is ready.
    drained: Option<Pin<Box<dyn Future<Output = ()> + Send + Sync>>>,
}

impl SnapshotWait {
    pub(crate) fn new(engine: Arc<RegolithEngine>) -> Self {
        Self {
            engine,
            drained: None,
        }
    }

    /// The pins live now. `0` means no snapshot, no snapshot-backed iterator
    /// and no open transaction holds a view of the database. Never waits. A
    /// snapshot being taken on another thread at this moment may or may not
    /// be counted.
    pub fn pinned(&self) -> u64 {
        self.engine.live_snapshot_count()
    }

    /// Whether no snapshot is pinned now. Never waits.
    pub fn is_ready(&self) -> bool {
        self.pinned() == 0
    }

    /// Block this thread until no snapshot is pinned or `timeout` passes, and
    /// return the pins still live: `0` means the wait succeeded. A blocking
    /// caller's way to wait; a task awaits the handle instead.
    ///
    /// It waits for the release itself, not on a polling interval, so it
    /// returns as soon as the last pin goes. The timeout is measured on the
    /// clock of [`Options::env`](crate::Options::env); where the environment
    /// has no clock the wait is one wait of `timeout`, ended early by the
    /// last release. On a target with a single thread (wasm without the
    /// `atomics` feature) nothing can release a pin while this thread waits,
    /// so it returns the count at once without waiting.
    pub fn wait(&self, timeout: Duration) -> u64 {
        self.engine.wait_for_snapshots(timeout)
    }
}

impl Future for SnapshotWait {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let engine = &this.engine;
        let drained = this.drained.get_or_insert_with(|| {
            let engine = Arc::clone(engine);
            Box::pin(async move { engine.snapshots_drained().await })
        });
        let polled = drained.as_mut().poll(cx);
        if polled.is_ready() {
            // A finished wait must not be polled again; the next poll makes
            // a new one.
            this.drained = None;
        }
        polled
    }
}

impl std::fmt::Debug for SnapshotWait {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotWait")
            .field("pinned", &self.pinned())
            .finish()
    }
}
