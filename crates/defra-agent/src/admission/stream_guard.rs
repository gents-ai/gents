use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use rig::completion::{CompletionError, GetTokenUsage, Usage};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamingCompletionResponse,
};

pub(crate) trait StreamGuardLifecycle {
    fn mark_stream_success(&mut self, _usage: Option<Usage>) {}

    fn mark_stream_error(&mut self, _error: &CompletionError) {}
}

pub(crate) fn hold_stream_guard<R, G>(
    stream: StreamingCompletionResponse<R>,
    guard: G,
) -> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage + Send + 'static,
    G: StreamGuardLifecycle + Send + Unpin + 'static,
{
    StreamingCompletionResponse::stream(Box::pin(GuardedStreamingResult {
        inner: stream,
        guard: Some(guard),
        pending: VecDeque::new(),
        message_id_emitted: false,
        done: false,
    }))
}

struct GuardedStreamingResult<R, G>
where
    R: Clone + Unpin + GetTokenUsage,
{
    inner: StreamingCompletionResponse<R>,
    guard: Option<G>,
    pending: VecDeque<RawStreamingChoice<R>>,
    message_id_emitted: bool,
    done: bool,
}

impl<R, G> GuardedStreamingResult<R, G>
where
    R: Clone + Unpin + GetTokenUsage,
{
    fn release_guard(&mut self) {
        drop(self.guard.take());
    }
}

impl<R, G> Stream for GuardedStreamingResult<R, G>
where
    R: Clone + Unpin + GetTokenUsage,
    G: StreamGuardLifecycle + Unpin,
{
    type Item = Result<RawStreamingChoice<R>, CompletionError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        if let Some(choice) = this.pending.pop_front() {
            return Poll::Ready(Some(Ok(choice)));
        }

        if this.done {
            return Poll::Ready(None);
        }

        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(item))) => {
                if let StreamedAssistantContent::Final(response) = &item {
                    if let Some(guard) = this.guard.as_mut() {
                        guard.mark_stream_success(response.token_usage());
                    }
                }
                this.pending = streamed_item_to_raw_choices(item).into();
                match this.pending.pop_front() {
                    Some(choice) => Poll::Ready(Some(Ok(choice))),
                    None => {
                        cx.waker().wake_by_ref();
                        Poll::Pending
                    }
                }
            }
            Poll::Ready(Some(Err(error))) => {
                if let Some(guard) = this.guard.as_mut() {
                    guard.mark_stream_error(&error);
                }
                this.release_guard();
                Poll::Ready(Some(Err(error)))
            }
            Poll::Ready(None) => {
                if let Some(guard) = this.guard.as_mut() {
                    guard.mark_stream_success(None);
                }
                this.release_guard();
                if !this.message_id_emitted {
                    this.message_id_emitted = true;
                    if let Some(message_id) = this.inner.message_id.clone() {
                        return Poll::Ready(Some(Ok(RawStreamingChoice::MessageId(message_id))));
                    }
                }
                this.done = true;
                Poll::Ready(None)
            }
        }
    }
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
