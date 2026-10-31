use anyhow::Result;

use super::{AgentDaemon, RequestInterruptionSource};
use crate::lifecycle::{RequestLifecycle, RequestTerminalOutcome, TerminalizeResult};
use crate::llm::rig_compat::ProviderModel;
use crate::runtime_trace::{record_current_failure_class, record_current_request_outcome};
use crate::streaming::DefraStreamWriter;
use crate::watcher::AgentRequest;

/// Only the winning terminal CAS authorizes follow-up effects.
pub(super) async fn terminalize_request(
    lifecycle: &mut RequestLifecycle,
    stream_writer: &DefraStreamWriter,
    outcome: RequestTerminalOutcome,
    reason: Option<&str>,
) -> Result<bool> {
    let selection = stream_writer
        .terminal_output(&lifecycle.request().doc_id)
        .await;
    match lifecycle
        .terminalize_owned(outcome, selection, reason)
        .await?
    {
        TerminalizeResult::Won => Ok(true),
        TerminalizeResult::AlreadySame => Ok(false),
        TerminalizeResult::Lost => anyhow::bail!(
            "request {} lost its execution generation before terminalization",
            lifecycle.request().request_id,
        ),
    }
}

/// The durable failure reason for a request error: the error and every cause,
/// joined as anyhow's `{:#}` joins them. Display alone keeps only the
/// outermost context (e.g. "projecting compactable provider messages") and
/// drops the cause that explains it. Each cause is redacted and capped, and
/// the whole reason is capped: a cause can carry a whole provider response
/// body, and the reason replicates.
pub(super) fn request_failure_reason(error: &anyhow::Error) -> String {
    crate::failure_text::bounded_error_chain(error.as_ref())
}

pub(super) async fn finalize_request_failure(
    lifecycle: &mut RequestLifecycle,
    stream_writer: &DefraStreamWriter,
    reason: &str,
    request_id: &str,
) -> bool {
    match terminalize_request(
        lifecycle,
        stream_writer,
        RequestTerminalOutcome::Failed,
        Some(reason),
    )
    .await
    {
        Ok(won) => won,
        Err(error) => {
            record_current_request_outcome("terminalization_failed");
            record_current_failure_class(&error);
            tracing::error!(request_id, error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                "failed to atomically terminalize request and response; durable lease recovery remains pending");
            false
        }
    }
}

impl<M> AgentDaemon<M>
where
    M: ProviderModel,
{
    /// A workspace-bound automated request's failure before owned work must
    /// release its Active writer binding; only the winning terminal CAS may.
    pub(super) async fn finalize_failure_before_work(
        &self,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        reason: &str,
        request: &AgentRequest,
    ) {
        if finalize_request_failure(lifecycle, stream_writer, reason, &request.request_id).await {
            self.release_failed_writer_binding(request).await;
        }
    }

    pub(super) async fn release_failed_writer_binding(&self, request: &AgentRequest) {
        if let Err(error) = crate::workspace::release_writer_binding(&self.node, request).await {
            tracing::warn!(
                request_id = %request.request_id,
                error = %crate::failure_text::BoundedErrorChain(error.as_ref()),
                "failed to release writer workspace binding after failure"
            );
        }
    }

    pub(super) async fn finalize_interrupt_before_work(
        &self,
        lifecycle: &mut RequestLifecycle,
        stream_writer: &DefraStreamWriter,
        request: &AgentRequest,
    ) -> Result<()> {
        if terminalize_request(
            lifecycle,
            stream_writer,
            RequestTerminalOutcome::Interrupted,
            Some("interrupted"),
        )
        .await?
        {
            record_current_request_outcome("interrupted");
            self.record_request_interruption(request, RequestInterruptionSource::Claimed);
            crate::workspace::release_writer_binding(&self.node, request).await?;
        }
        Ok(())
    }

    pub(super) fn record_request_interruption(
        &self,
        request: &AgentRequest,
        source: RequestInterruptionSource,
    ) {
        tracing::info!(
            target: "gents::agent::daemon",
            agent_id = %self.agent_config.agent_id,
            request_id = %request.request_id,
            session_id = %request.session_id,
            cancellation_source = source.as_str(),
            "request interrupted"
        );
    }
}
