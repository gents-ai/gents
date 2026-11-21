use std::time::Duration;

use anyhow::{Context, Result};
use gents_pack::plugin::store::{PublishedPluginReadError, StoreGuard, StoreReadPin};
use tokio::time::Instant;

pub const PLUGIN_PUBLICATION_WAIT_EVENT_TARGET: &str = "gents_tool_surface_publication";

pub const PUBLICATION_WAIT: Duration = Duration::from_secs(60);
const PUBLICATION_POLL: Duration = Duration::from_millis(100);

/// Lean `ToolPolicy.PluginPublication.retry`.
fn retry(busy: bool, now: Instant, deadline: Instant) -> bool {
    busy && now < deadline
}

/// Lean `ToolPolicy.PluginPublication.nextAttempt`.
fn next_attempt(now: Instant, deadline: Instant, poll: Duration) -> Instant {
    deadline.min(now + poll)
}

/// The store's shared publication lease, taken through the runtime's pinned
/// identity and waited for while an installer holds the store, until
/// `deadline`. A runtime without a plugin home needs none.
pub async fn publication_guard_until(
    pin: Option<&StoreReadPin>,
    deadline: Instant,
) -> std::result::Result<Option<StoreGuard>, PublishedPluginReadError> {
    let Some(pin) = pin else {
        return Ok(None);
    };
    let mut waiting = false;
    loop {
        match pin.try_shared() {
            Ok(guard) => return Ok(Some(guard)),
            Err(error) if retry(error.is_busy(), Instant::now(), deadline) => {
                if !waiting {
                    tracing::warn!(target: PLUGIN_PUBLICATION_WAIT_EVENT_TARGET, cause = %error, "waiting for plugin publication before constructing tool surface");
                    waiting = true;
                }
                tokio::time::sleep_until(next_attempt(Instant::now(), deadline, PUBLICATION_POLL))
                    .await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// The filesystem lock has no cross-process notification, so a construction
/// that met an installer's lock polls it. Only that typed contention is
/// retried, never a missing plugin or a pin mismatch.
pub(super) async fn wait<T>(
    deadline: Instant,
    mut construct: impl FnMut() -> Result<T>,
) -> Result<T> {
    let mut retries = 0_u64;
    loop {
        match construct() {
            Ok(built) => return Ok(built),
            Err(error) => {
                let busy = PublishedPluginReadError::error_chain_is_busy(error.as_ref());
                let now = Instant::now();
                if !retry(busy, now, deadline) {
                    return if busy {
                        Err(error).with_context(|| {
                            format!("plugin publication wait exhausted after {retries} retries")
                        })
                    } else {
                        Err(error)
                    };
                }
                if retries == 0 {
                    tracing::warn!(target: PLUGIN_PUBLICATION_WAIT_EVENT_TARGET, cause = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "waiting for plugin publication before constructing tool surface");
                }
                retries += 1;
                tokio::time::sleep_until(next_attempt(now, deadline, PUBLICATION_POLL)).await;
            }
        }
    }
}

#[cfg(test)]
#[path = "publication_tests.rs"]
mod tests;
