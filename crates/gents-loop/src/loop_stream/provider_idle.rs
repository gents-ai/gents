//! Provider idle window at the attempt seam. A provider silent at the
//! transport is a transport-class attempt failure handed to the completion
//! retry owner; it never becomes a separate retry path.

use std::future::Future;
use std::time::Duration;

use rig::agent::StreamingError;
use rig::completion::CompletionError;

use crate::error::InferenceError;
use crate::provider_activity::{ActivityPhase, ProviderActivity};

#[derive(Debug)]
pub(super) struct ProviderStall {
    idle: Duration,
    first_item: bool,
}

impl ProviderStall {
    fn reason(&self) -> String {
        format!(
            "provider stream stalled: no transport activity for {:?} {}",
            self.idle,
            if self.first_item {
                "before the first item"
            } else {
                "after the last item"
            }
        )
    }
}

pub(super) enum ProviderAttemptFailure {
    Completion(CompletionError),
    Stalled(ProviderStall),
}

impl ProviderAttemptFailure {
    pub(super) fn classify(self) -> (InferenceError, String) {
        match self {
            Self::Completion(error) => {
                let error = StreamingError::Completion(error);
                (
                    crate::error::classify_completion_error(&error),
                    error.to_string(),
                )
            }
            Self::Stalled(stall) => (
                InferenceError::Timeout {
                    timeout: stall.idle,
                },
                stall.reason(),
            ),
        }
    }
}

/// Drive one provider future (the stream constructor or one `next`) while
/// the attempt's transport is idle for less than `idle`. Without a window or
/// an armed attempt, the future runs unbounded. Expiry drops only `future`;
/// the caller must drop the attempt's stream before any retry backoff so its
/// admission permit is released.
pub(super) async fn within_provider_idle<F: Future>(
    future: F,
    idle: Option<Duration>,
    activity: Option<&ProviderActivity>,
    first_item: bool,
) -> Result<F::Output, ProviderAttemptFailure> {
    let (Some(idle), Some(activity)) = (idle, activity) else {
        return Ok(future.await);
    };
    tokio::pin!(future);
    loop {
        let check_at = match activity.phase() {
            ActivityPhase::Active(last) => last + idle,
            ActivityPhase::AwaitingSend | ActivityPhase::Settled => {
                tokio::time::Instant::now() + idle
            }
        };
        tokio::select! {
            biased;
            output = &mut future => return Ok(output),
            _ = tokio::time::sleep_until(check_at) => {
                if let ActivityPhase::Active(last) = activity.phase() {
                    if last.elapsed() >= idle {
                        let stall = ProviderStall { idle, first_item };
                        activity.mark_stalled(stall.reason());
                        return Err(ProviderAttemptFailure::Stalled(stall));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "provider_idle_tests.rs"]
mod tests;
