use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum HostCallError {
    #[error("the plugin invocation stopped")]
    Stopped,
    #[error("plugin worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
    #[error(transparent)]
    Execution(#[from] anyhow::Error),
}

pub struct Call {
    pub bytes: Vec<u8>,
    pub reply: oneshot::Sender<Vec<u8>>,
}

/// What the invocation and its detached WASM worker share about stopping.
pub struct Stop {
    pub stopped: AtomicBool,
    pub revoked: AtomicBool,
    pub deadline: std::time::Instant,
    pub slice: Duration,
}

/// Held by the async invocation: returning or being dropped (a timeout, a
/// failed child, cancellation, interrupt, lifetime expiry) stops the guest.
pub struct StopOnDrop(pub Arc<Stop>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
    }
}

/// Only the blocking WASM worker uses this end. The receiver and every reply
/// are owned by the async invocation, so dropping it releases a suspended guest.
#[derive(Clone)]
pub struct Bridge {
    pub calls: mpsc::Sender<Call>,
    pub stop: Arc<Stop>,
}

impl Bridge {
    pub fn call(&self, bytes: Vec<u8>) -> Result<Vec<u8>, HostCallError> {
        if self.stop.stopped.load(Ordering::Acquire) {
            return Err(HostCallError::Stopped);
        }
        let (reply, answer) = oneshot::channel();
        // A closed channel or a dropped reply is the invocation stopping; the
        // flag is set here because the worker can observe either first.
        self.calls
            .blocking_send(Call { bytes, reply })
            .map_err(|_| self.stopped())?;
        answer.blocking_recv().map_err(|_| self.stopped())
    }

    fn stopped(&self) -> HostCallError {
        self.stop.stopped.store(true, Ordering::Release);
        HostCallError::Stopped
    }

    /// The compute the guest may spend before its next host call: nothing
    /// once the invocation stopped, so a guest that ignores the failed call
    /// traps at its next epoch check instead of running on. The pinned VM
    /// exposes a store only inside a host import, so a guest that never calls
    /// the host again is stopped by this allowance, not at the stop itself.
    /// Compiled code rereads the deadline at each function entry and, inside
    /// a loop, only once the deadline it cached has lapsed; a revocation
    /// therefore lands at the guest's next function entry.
    ///
    /// `None` leaves the store's deadline alone: a deadline is relative to
    /// the engine's current epoch, so revoking it again on every later call
    /// would keep moving it ahead of a guest that calls in a tight loop.
    pub fn compute_allowance(&self) -> Option<Duration> {
        let stopped = self.stop.stopped.load(Ordering::Acquire);
        if stopped && self.stop.revoked.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some(compute_allowance(
            self.stop.slice,
            self.stop
                .deadline
                .saturating_duration_since(std::time::Instant::now()),
            stopped,
        ))
    }
}

pub fn compute_allowance(declared: Duration, remaining: Duration, stopped: bool) -> Duration {
    if stopped {
        Duration::ZERO
    } else {
        declared.min(remaining)
    }
}
