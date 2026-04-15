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
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use futures::StreamExt;
    use rig::completion::CompletionResponse;
    use rig::streaming::{RawStreamingChoice, StreamingCompletionResponse};

    use super::hold_stream_guard;

    struct DropProbe {
        drops: Arc<AtomicUsize>,
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl super::StreamGuardLifecycle for DropProbe {}

    #[tokio::test]
    async fn holds_guard_until_stream_eof_and_preserves_final_response_metadata() {
        let drops = Arc::new(AtomicUsize::new(0));
        let inner = StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![
            Ok(RawStreamingChoice::Message("hello".to_string())),
            Ok(RawStreamingChoice::MessageId("msg_123".to_string())),
            Ok(RawStreamingChoice::FinalResponse(())),
        ])));
        let mut guarded = hold_stream_guard(
            inner,
            DropProbe {
                drops: drops.clone(),
            },
        );

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        while guarded.next().await.is_some() {
            assert_eq!(drops.load(Ordering::SeqCst), 0);
        }

        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let completed: CompletionResponse<Option<()>> = guarded.into();
        assert_eq!(completed.raw_response, Some(()));
        assert_eq!(completed.message_id.as_deref(), Some("msg_123"));
    }

    #[tokio::test]
    async fn drops_guard_when_caller_drops_stream_before_eof() {
        let drops = Arc::new(AtomicUsize::new(0));
        let inner: StreamingCompletionResponse<()> =
            StreamingCompletionResponse::stream(Box::pin(futures::stream::pending()));
        let guarded = hold_stream_guard(
            inner,
            DropProbe {
                drops: drops.clone(),
            },
        );

        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(guarded);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn drops_guard_when_inner_stream_errors() {
        let drops = Arc::new(AtomicUsize::new(0));
        let inner: StreamingCompletionResponse<()> =
            StreamingCompletionResponse::stream(Box::pin(futures::stream::iter(vec![Err(
                rig::completion::CompletionError::ProviderError("boom".to_string()),
            )])));
        let mut guarded = hold_stream_guard(
            inner,
            DropProbe {
                drops: drops.clone(),
            },
        );

        let item = guarded.next().await.expect("error item");
        assert!(item.is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
