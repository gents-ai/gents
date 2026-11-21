use std::time::Duration;

use anyhow::{Context, Result};
use gents_pack::plugin::store::{
    next_attempt, retry, wait_for_lease, PublishedPluginReadError, StoreGuard, StoreReadPin,
};
use tokio::time::Instant;

pub const PLUGIN_PUBLICATION_WAIT_EVENT_TARGET: &str = "gents_tool_surface_publication";

pub const PUBLICATION_WAIT: Duration = gents_pack::plugin::store::LEASE_WAIT;
const PUBLICATION_POLL: Duration = Duration::from_millis(100);

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
    wait_for_lease(deadline, || {
        let lease = pin.try_shared();
        if !waiting && lease.as_ref().is_err_and(PublishedPluginReadError::is_busy) {
            tracing::warn!(target: PLUGIN_PUBLICATION_WAIT_EVENT_TARGET, cause = %PublishedPluginReadError::Busy, "waiting for plugin publication before constructing tool surface");
            waiting = true;
        }
        lease
    })
    .await
    .map(Some)
}

/// A construction that met an installer's lock polls it. Only that typed
/// contention is retried, never a missing plugin or a pin mismatch.
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
