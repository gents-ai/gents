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

/// The stream consumer also performs response writes. It may stop polling
/// `next()` to await one of those writes while finalization owns the same gate.
/// Independently schedule the owner so that both storage completion and its
/// timeout continue to be polled. This does not make storage preemptible.
/// The stream still owns cancellation: dropping it aborts this worker and lets
/// the existing guard Drop path perform terminal repair.
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
