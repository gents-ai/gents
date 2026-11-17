use std::time::Duration;

use anyhow::{Context, Result};
use gents_pack::plugin::store::PublishedPluginReadError;
use tokio::time::Instant;

pub const PLUGIN_PUBLICATION_WAIT_EVENT_TARGET: &str = "gents_tool_surface_publication";

pub(super) const PUBLICATION_WAIT: Duration = Duration::from_secs(60);
const PUBLICATION_POLL: Duration = Duration::from_millis(100);

/// A runtime rebuild takes its configuration snapshot and plugin reads on
/// the same side of an install's publication fence.
pub async fn publication_guard(
    home: Option<&std::path::Path>,
) -> Result<Option<gents_pack::plugin::store::StoreGuard>> {
    publication_guard_until(home, Instant::now() + PUBLICATION_WAIT).await
}

pub async fn publication_guard_until(
    home: Option<&std::path::Path>,
    deadline: Instant,
) -> Result<Option<gents_pack::plugin::store::StoreGuard>> {
    let Some(home) = home else {
        return Ok(None);
    };
    wait(deadline, || {
        gents_pack::plugin::store::try_lock_store_shared(home).map_err(Into::into)
    })
    .await
    .map(Some)
}

fn may_retry(busy: bool, budget_available: bool) -> bool {
    busy && budget_available
}

/// The filesystem lock has no cross-process async notification. Poll only
/// that lock's typed contention, never a database error or a pin mismatch.
pub(super) async fn wait<T>(
    deadline: Instant,
    mut construct: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut attempts = 0_u64;
    loop {
        match construct() {
            Ok(tool) => return Ok(tool),
            Err(error) => {
                let now = Instant::now();
                let busy = PublishedPluginReadError::error_chain_is_busy(error.as_ref());
                if !may_retry(busy, now < deadline) {
                    return if busy {
                        Err(error).with_context(|| {
                            format!("plugin publication wait exhausted after {attempts} retries")
                        })
                    } else {
                        Err(error)
                    };
                }
                if attempts == 0 {
                    tracing::warn!(target: PLUGIN_PUBLICATION_WAIT_EVENT_TARGET, cause = %format!("{error:#}"), "waiting for plugin publication before constructing tool surface");
                }
                attempts += 1;
                tokio::time::sleep_until(deadline.min(now + PUBLICATION_POLL)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_retry_replays_the_lean_owner() {
        gents_lean_vocab::families::tool_policy_plugin_publication_retry::replay(|i| {
            may_retry(i.busy, i.budget_available)
        });
    }

    #[tokio::test(start_paused = true)]
    async fn publication_wait_is_bounded_and_preserves_the_busy_cause() {
        let mut attempts = 0;
        let error = wait(Instant::now() + Duration::from_millis(200), || {
            attempts += 1;
            Err::<(), _>(PublishedPluginReadError::Busy.into())
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 3);
        assert!(PublishedPluginReadError::error_chain_is_busy(
            error.as_ref()
        ));
        assert!(format!("{error:#}").contains("wait exhausted after 2 retries"));
    }

    #[tokio::test(start_paused = true)]
    async fn publication_wait_does_not_retry_a_permanent_failure() {
        let mut attempts = 0;
        wait(Instant::now() + PUBLICATION_WAIT, || {
            attempts += 1;
            Err::<(), _>(anyhow::anyhow!("pinned digest does not match"))
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn publication_wait_reconstructs_after_the_writer_releases() {
        let mut attempts = 0;
        let tool = wait(Instant::now() + PUBLICATION_WAIT, || {
            attempts += 1;
            if attempts == 1 {
                Err(PublishedPluginReadError::Busy.into())
            } else {
                Ok("exact pinned tool")
            }
        })
        .await
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(tool, "exact pinned tool");
    }
}
