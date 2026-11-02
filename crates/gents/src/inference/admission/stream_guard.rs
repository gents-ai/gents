use futures::future::BoxFuture;
use gents_loop::rig_compat::CachedInputTokensObservation;
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;

use super::{AdmissionError, ProviderCallUsage};

pub(crate) trait StreamGuardLifecycle {
    fn mark_stream_success(
        &mut self,
        _usage: Option<ProviderCallUsage>,
        _cached_input_tokens: CachedInputTokensObservation,
    ) {
    }

    fn mark_stream_error(&mut self, _error: &str) {}

    fn finish_stream(self) -> BoxFuture<'static, Result<(), AdmissionError>>
    where
        Self: Sized + Send + 'static,
    {
        Box::pin(async { Ok(()) })
    }
}

/// The stream consumer may stop polling `next()` while finalization owns the
/// same write gate, so the owner runs independently; dropping the stream still
/// aborts it and lets the guard's Drop path perform terminal repair.
pub(crate) async fn finish_guard<G>(guard: G) -> Result<(), AdmissionError>
where
    G: StreamGuardLifecycle + Send + 'static,
{
    AbortOnDropHandle::new(tokio::spawn(
        guard.finish_stream().instrument(tracing::Span::current()),
    ))
    .await
    .map_err(|error| {
        AdmissionError(format!(
            "inference-call finalization worker failed: {error}"
        ))
    })?
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod conformance;
