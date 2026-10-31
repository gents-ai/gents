use futures::StreamExt;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(rename_all = "snake_case"))]
pub(super) enum Change {
    Quiet,
    Unrelated,
    Relevant,
    Overflow,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(rename_all = "snake_case"))]
pub(super) enum Wake {
    Cancel,
    Deadline,
    Rescan,
    Changed,
    Wait,
}

/// `Proofs.Background.Observation` owns wake priority and closure latching.
/// The host must poll ready timers and finish each durable read; notifications
/// supply invalidations, never lifecycle or authorization facts.
pub(super) struct Observation {
    rescan_at: Instant,
    feed_open: bool,
}

impl Observation {
    pub(super) fn new(rescan_at: Instant, feed_open: bool) -> Self {
        Self {
            rescan_at,
            feed_open,
        }
    }

    pub(super) fn rescan_at(&self) -> Instant {
        self.rescan_at
    }

    pub(super) fn feed_open(&self) -> bool {
        self.feed_open
    }

    pub(super) fn wake(
        &self,
        now: Instant,
        deadline_at: Instant,
        cancelled: bool,
        change: Change,
    ) -> Wake {
        if cancelled {
            Wake::Cancel
        } else if now >= deadline_at {
            Wake::Deadline
        } else if now >= self.rescan_at {
            Wake::Rescan
        } else if self.feed_open && !matches!(change, Change::Quiet | Change::Unrelated) {
            Wake::Changed
        } else {
            Wake::Wait
        }
    }

    pub(super) fn observe_change(&mut self, change: Change) {
        self.feed_open &= change != Change::Closed;
    }

    pub(super) async fn wait<S, F>(
        &mut self,
        changes: &mut S,
        deadline_at: Instant,
        interval: std::time::Duration,
        relevant: F,
    ) where
        S: futures::Stream<Item = events::DocumentChangeBatch> + Unpin,
        F: Fn(&events::DocumentChangeBatch) -> bool,
    {
        loop {
            let change = tokio::select! {
                biased;
                _ = tokio::time::sleep_until(deadline_at) => Change::Quiet,
                _ = tokio::time::sleep_until(self.rescan_at()) => Change::Quiet,
                batch = changes.next(), if self.feed_open() => match batch {
                    None => Change::Closed,
                    Some(batch) if batch.resync_required => Change::Overflow,
                    Some(batch) if relevant(&batch) => Change::Relevant,
                    Some(_) => Change::Unrelated,
                },
            };
            let now = Instant::now();
            let wake = self.wake(now, deadline_at, false, change);
            self.observe_change(change);
            match wake {
                Wake::Rescan => {
                    self.rescan_at = now + interval;
                    return;
                }
                Wake::Cancel | Wake::Deadline | Wake::Changed => return,
                Wake::Wait => tokio::task::yield_now().await,
            }
        }
    }
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
