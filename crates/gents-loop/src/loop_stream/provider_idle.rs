use std::future::Future;
use std::time::Duration;

use rig::agent::StreamingError;
use rig::completion::CompletionError;

use crate::error::{ClassifiedFailure, InferenceError, ProviderReport};
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
    pub(super) fn classify(self) -> ClassifiedFailure {
        match self {
            Self::Completion(error) => {
                crate::rig_compat::classify_completion_error(&StreamingError::Completion(error))
            }
            Self::Stalled(stall) => ClassifiedFailure {
                error: InferenceError::Timeout {
                    timeout: stall.idle,
                },
                report: ProviderReport::default(),
                text: stall.reason(),
            },
        }
    }
}

pub(super) async fn within_provider_idle<F>(
    future: F,
    idle: Option<Duration>,
    activity: Option<&ProviderActivity>,
    first_item: bool,
) -> Result<F::Output, ProviderAttemptFailure>
where
    F: Future,
{
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
