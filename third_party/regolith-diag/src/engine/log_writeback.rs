//! The log writeback lane: a thread that hands the active write-ahead log
//! to the device's writeback as it fills, so the rotation that seals it
//! finds little left to sync (E46).
//!
//! A rotation makes the log it seals durable before the next log exists
//! (E2, `Ordered::seal`), inside the ordered step where every writer waits
//! for it. Synced whole there, a log of a full write buffer stalls every
//! writer for as long as the host takes to write it out. So each log calls
//! a kick each time it grows by an eighth of the write buffer
//! (`wal_log::EarlyWriteback`), and this lane starts the writeback of what
//! writers passed ([`crate::env::LogFile::start_writeback`]): no sync, no
//! claim on the pipeline, no durability promised, only pages the seal's
//! sync then finds written. On Linux that is `sync_file_range`; on Apple,
//! whose `sync_data` is `F_FULLFSYNC` (the pages and the drive's cache), it
//! is a plain `fsync`, which writes the pages out without flushing the drive
//! cache, so the seal's `F_FULLFSYNC` finds them written. Where the host has
//! neither, there is no lane and the seal writes the log as before.
//!
//! Off unless the caller turns it on ([`crate::Options::log_writeback`]):
//! the lane is a thread beyond the compaction threads the database
//! otherwise keeps to (plan 3.13 and 4.5), so asking for it is the caller's
//! call. Off, no log has an early writeback, and a record's write costs one
//! branch on `None`.
//!
//! A thread of its own, not a job for the compaction workers: a worker is
//! busy for the length of a flush or a compaction pass under exactly the
//! load that rotates, and writeback waiting behind one is writeback the
//! seal does itself. Not on the writer's thread either: starting writeback
//! costs the caller the host's work of writing the pages out, compression
//! included on a filesystem that compresses, which the writer would pay on
//! the commit path. Not a durable sync of the log ahead either: on btrfs a
//! sync holds the file against writes while it runs, and syncing ahead
//! measured slower there than the one sync at the seal. Apple's `fsync` is
//! no durable sync, since it leaves the drive's cache; its effect on the
//! seal there has not been measured.
//!
//! # Invariants
//!
//! - **One writeback asked for at a time.** A kick while one is asked for
//!   and not begun is dropped: the one that runs covers what that kick asked
//!   for too.
//! - **Nothing waits for the lane.** A kick is a flag and a channel send;
//!   writeback the lane has not started is writeback the seal does.
//! - **Only a kick starts writeback.** An idle database is not touched on
//!   a timer.

use std::io;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use kovan_channel::RecvDeadline;
use kovan_channel::unbounded::Sender;
use kovan_queue::seg_queue::SegQueue;

use crate::env::{Env, JoinHandle};
use crate::portability::{AtomicBool, Ordering};

/// How long the lane waits for a kick before it looks at the shutdown flag
/// on its own.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// What the lane runs when kicked: the engine's writeback of the active log.
type Hook = Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>;

/// The lane: its thread, and how to wake it. See the module docs.
pub(crate) struct LogWritebackLane {
    trigger: Sender<()>,
    /// A kick is waiting for the lane: no second one is sent.
    asked: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    hook: Hook,
    /// The thread, popped by whichever shutdown comes first.
    handle: SegQueue<Box<dyn JoinHandle>>,
}

impl LogWritebackLane {
    /// Start the lane's thread through `env`.
    pub(crate) fn start(env: &dyn Env) -> io::Result<Self> {
        let (trigger, receiver) = kovan_channel::unbounded::<()>();
        let asked = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(AtomicBool::new(false));
        let hook: Hook = Arc::new(OnceLock::new());
        let handle = {
            let asked = Arc::clone(&asked);
            let shutdown = Arc::clone(&shutdown);
            let hook = Arc::clone(&hook);
            env.spawn(
                "regolith-log-writeback",
                Box::new(move || {
                    loop {
                        let kicked = match receiver.recv_deadline(Instant::now() + POLL_INTERVAL) {
                            RecvDeadline::Msg(()) => {
                                asked.store(false, Ordering::Release);
                                true
                            }
                            RecvDeadline::Timeout => false,
                            RecvDeadline::Disconnected => break,
                        };
                        if shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        if kicked && let Some(hook) = hook.get() {
                            hook();
                        }
                    }
                }),
            )
            .map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!(
                        "failed to spawn the log writeback thread: {e}; set \
                         Options::max_background_compactions = 0 to run without background threads"
                    ),
                )
            })?
        };
        let slot = SegQueue::new();
        slot.push(handle);
        Ok(Self {
            trigger,
            asked,
            shutdown,
            hook,
            handle: slot,
        })
    }

    /// Install what a kick runs. The first call wins; the engine makes one.
    pub(crate) fn set_hook(&self, hook: Box<dyn Fn() + Send + Sync>) {
        let _ = self.hook.set(hook);
    }

    /// What a log calls to ask the lane for writeback: a flag and, when no
    /// kick is waiting already, one channel send. Holds nothing of the
    /// engine.
    pub(crate) fn kicker(&self) -> Arc<dyn Fn() + Send + Sync> {
        let trigger = self.trigger.clone();
        let asked = Arc::clone(&self.asked);
        Arc::new(move || {
            if !asked.swap(true, Ordering::AcqRel) {
                trigger.send(());
            }
        })
    }

    /// Stop the lane and wait for its thread. Writeback under way finishes
    /// first. Idempotent.
    pub(crate) fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        self.trigger.send(());
        if let Some(handle) = self.handle.pop() {
            handle.join();
        }
    }
}

impl Drop for LogWritebackLane {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
#[path = "log_writeback_tests.rs"]
mod tests;
