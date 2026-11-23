//! Waiting for a plugin store lease another holder has. The filesystem lock
//! has no cross-process notification, so a waiter polls it, and only that
//! typed contention is waited out: never an absent or replaced store.
use std::time::Duration;

use tokio::time::Instant;

use super::{PublishedPluginReadError, StoreGuard, StoreWriter};

/// How long a writer waits for the runtime's readers, and they for it.
pub const LEASE_WAIT: Duration = Duration::from_secs(60);
const LEASE_POLL: Duration = Duration::from_millis(100);

/// Lean `ToolPolicy.PluginPublication.retry`.
pub fn retry(busy: bool, now: Instant, deadline: Instant) -> bool {
    busy && now < deadline
}

/// Lean `ToolPolicy.PluginPublication.nextAttempt`.
pub fn next_attempt(now: Instant, deadline: Instant, poll: Duration) -> Instant {
    deadline.min(now + poll)
}

/// Polls `attempt` while it reports contention and `deadline` is ahead.
pub async fn wait_for_lease<T>(
    deadline: Instant,
    mut attempt: impl FnMut() -> Result<T, PublishedPluginReadError>,
) -> Result<T, PublishedPluginReadError> {
    loop {
        match attempt() {
            Err(error) if retry(error.is_busy(), Instant::now(), deadline) => {
                tokio::time::sleep_until(next_attempt(Instant::now(), deadline, LEASE_POLL)).await;
            }
            result => return result,
        }
    }
}

impl StoreWriter {
    /// The exclusive writer lease, waiting for readers until `deadline`.
    pub async fn lock_until(&self, deadline: Instant) -> anyhow::Result<StoreGuard> {
        Ok(wait_for_lease(deadline, || self.try_lock_typed()).await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lease_wait_replays_the_lean_owner() {
        let base = Instant::now();
        let at = |millis: u64| base + Duration::from_millis(millis);
        gents_lean_vocab::families::plugin_publication_retry::replay(|i| {
            retry(i.busy, at(i.now), at(i.deadline))
        });
        gents_lean_vocab::families::plugin_publication_next_attempt::replay(|i| {
            let next = next_attempt(at(i.now), at(i.deadline), Duration::from_millis(i.poll));
            u64::try_from((next - base).as_millis()).unwrap()
        });
    }

    #[tokio::test]
    async fn a_writer_waits_for_a_reader_and_then_locks() {
        let home = tempfile::tempdir().unwrap();
        super::super::bootstrap_store(home.path()).unwrap();
        let reader = super::super::try_lock_store_shared(home.path()).unwrap();
        let writer = StoreWriter::Offline(home.path().to_owned());
        let locking = tokio::spawn(async move {
            writer
                .lock_until(Instant::now() + LEASE_WAIT)
                .await
                .map(drop)
        });
        tokio::task::yield_now().await;
        drop(reader);
        locking.await.unwrap().unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn the_wait_ends_at_its_deadline_with_the_busy_cause() {
        let mut attempts = 0;
        let error = wait_for_lease(Instant::now() + Duration::from_millis(200), || {
            attempts += 1;
            Err::<(), _>(PublishedPluginReadError::Busy)
        })
        .await
        .unwrap_err();
        assert!(error.is_busy());
        assert_eq!(attempts, 3);
    }
}
