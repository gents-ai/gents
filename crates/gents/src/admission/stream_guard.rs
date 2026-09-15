use async_stream::try_stream;
use futures::{future::BoxFuture, StreamExt};
use rig::completion::{CompletionError, GetTokenUsage, Usage};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamingCompletionResponse,
};
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument;

pub(crate) trait StreamGuardLifecycle {
    fn mark_stream_success(&mut self, _usage: Option<Usage>) {}

    fn mark_stream_error(&mut self, _error: &CompletionError) {}

    fn finish_stream(self) -> BoxFuture<'static, Result<(), CompletionError>>
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
async fn finish_guard<G>(guard: G) -> Result<(), CompletionError>
where
    G: StreamGuardLifecycle + Send + 'static,
{
    AbortOnDropHandle::new(tokio::spawn(
        guard.finish_stream().instrument(tracing::Span::current()),
    ))
    .await
    .map_err(|error| {
        CompletionError::ProviderError(format!(
            "inference-call finalization worker failed: {error}"
        ))
    })?
}

pub(crate) fn hold_stream_guard<R, G>(
    stream: StreamingCompletionResponse<R>,
    guard: G,
) -> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
    G: StreamGuardLifecycle + Send + Unpin + 'static,
{
    let guarded = try_stream! {
        let mut inner = stream;
        let mut guard = Some(guard);
        while let Some(item) = inner.next().await {
            match item {
                Ok(item) => {
                    if let StreamedAssistantContent::Final(response) = &item {
                        let mut terminal_guard = guard
                            .take()
                            .expect("stream guard finalization starts exactly once");
                        terminal_guard.mark_stream_success(response.token_usage());
                        // The owned loop charges from the terminal item, while
                        // crash rehydrate charges from the InferenceCall row.
                        // Persist before publishing so the two cannot diverge.
                        finish_guard(terminal_guard).await?;
                    }
                    for choice in streamed_item_to_raw_choices(item) {
                        yield choice;
                    }
                }
                Err(error) => {
                    if let Some(mut terminal_guard) = guard.take() {
                        terminal_guard.mark_stream_error(&error);
                        finish_guard(terminal_guard).await?;
                    }
                    Err(error)?;
                }
            }
        }
        if let Some(mut terminal_guard) = guard.take() {
            terminal_guard.mark_stream_success(None);
            finish_guard(terminal_guard).await?;
        }
        if let Some(message_id) = inner.message_id {
            yield RawStreamingChoice::MessageId(message_id);
        }
    };
    StreamingCompletionResponse::stream(Box::pin(guarded))
}

fn streamed_item_to_raw_choices<R>(item: StreamedAssistantContent<R>) -> Vec<RawStreamingChoice<R>>
where
    R: Clone,
{
    match item {
        StreamedAssistantContent::Text(text) => vec![RawStreamingChoice::Message(text.text)],
        StreamedAssistantContent::ToolCall {
            tool_call,
            internal_call_id,
        } => vec![RawStreamingChoice::ToolCall(RawStreamingToolCall {
            id: tool_call.id,
            internal_call_id,
            call_id: tool_call.call_id,
            name: tool_call.function.name,
            arguments: tool_call.function.arguments,
            signature: tool_call.signature,
            additional_params: tool_call.additional_params,
        })],
        StreamedAssistantContent::ToolCallDelta {
            id,
            internal_call_id,
            content,
        } => vec![RawStreamingChoice::ToolCallDelta {
            id,
            internal_call_id,
            content,
        }],
        StreamedAssistantContent::Reasoning(reasoning) => reasoning
            .content
            .into_iter()
            .map(|content| RawStreamingChoice::Reasoning {
                id: reasoning.id.clone(),
                content,
            })
            .collect(),
        StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
            vec![RawStreamingChoice::ReasoningDelta { id, reasoning }]
        }
        StreamedAssistantContent::Final(response) => {
            vec![RawStreamingChoice::FinalResponse(response)]
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod conformance;
