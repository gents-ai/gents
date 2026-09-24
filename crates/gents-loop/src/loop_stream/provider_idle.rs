//! Provider idle window at the attempt seam. A silent provider stream is a
//! transport-class attempt failure handed to the completion retry owner; it
//! never becomes a separate retry path.

use futures::{Stream, StreamExt};
use rig::agent::StreamingError;
use rig::completion::CompletionError;

use crate::error::InferenceError;

pub(super) enum ProviderAttemptFailure {
    Completion(CompletionError),
    Stalled {
        idle: std::time::Duration,
        first_item: bool,
    },
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
            Self::Stalled { idle, first_item } => (
                InferenceError::Timeout {
                    timeout_secs: idle.as_secs(),
                },
                format!(
                    "provider stream stalled: no {} within {idle:?}",
                    if first_item {
                        "first item"
                    } else {
                        "further item"
                    }
                ),
            ),
        }
    }
}

/// Expiry drops only the pending `next`; every item received before it was
/// already returned to the loop. The caller keeps the stream alive while it
/// reports the attempt failure or retraction and must not poll it again.
pub(super) async fn next_provider_item<S, T>(
    stream: &mut S,
    idle: Option<std::time::Duration>,
    saw_item: bool,
) -> Option<Result<T, ProviderAttemptFailure>>
where
    S: Stream<Item = Result<T, CompletionError>> + Unpin,
{
    let next = match idle {
        Some(idle) => match tokio::time::timeout(idle, stream.next()).await {
            Ok(next) => next,
            Err(_) => {
                return Some(Err(ProviderAttemptFailure::Stalled {
                    idle,
                    first_item: !saw_item,
                }))
            }
        },
        None => stream.next().await,
    };
    next.map(|item| item.map_err(ProviderAttemptFailure::Completion))
}

#[cfg(test)]
#[path = "provider_idle_tests.rs"]
mod tests;
