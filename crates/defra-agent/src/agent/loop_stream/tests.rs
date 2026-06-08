use std::sync::Arc;

use futures::{stream, StreamExt};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Message};
use rig::streaming::{RawStreamingChoice, StreamedAssistantContent, StreamingCompletionResponse};

use super::*;

/// A `CompletionModel` whose `stream` replays a fixed script of
/// `RawStreamingChoice`s, so loop behavior can be tested without a provider.
#[derive(Clone)]
struct ScriptedModel {
    chunks: Arc<Vec<RawStreamingChoice<()>>>,
}

impl ScriptedModel {
    fn new(chunks: Vec<RawStreamingChoice<()>>) -> Self {
        Self {
            chunks: Arc::new(chunks),
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for ScriptedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        Self {
            chunks: Arc::new(Vec::new()),
        }
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        Err(CompletionError::ProviderError(
            "completion is unused in loop_stream tests".to_string(),
        ))
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let items: Vec<Result<RawStreamingChoice<()>, CompletionError>> =
            self.chunks.iter().cloned().map(Ok).collect();
        Ok(StreamingCompletionResponse::stream(Box::pin(stream::iter(
            items,
        ))))
    }
}

fn config(max_turns: usize) -> LoopConfig {
    LoopConfig {
        preamble: None,
        temperature: None,
        max_tokens: None,
        additional_params: None,
        tool_choice: None,
        max_turns,
    }
}

#[tokio::test]
async fn single_turn_no_tools_yields_text_then_final() {
    let model = ScriptedModel::new(vec![
        RawStreamingChoice::Message("Hello ".to_string()),
        RawStreamingChoice::Message("world".to_string()),
        RawStreamingChoice::FinalResponse(()),
    ]);

    let stream = run_loop_stream(
        model,
        Message::user("hi"),
        Vec::new(),
        Vec::new(),
        config(0),
    );
    futures::pin_mut!(stream);

    let mut texts = Vec::new();
    let mut final_text = None;
    while let Some(item) = stream.next().await {
        match item.expect("loop item should be Ok") {
            LoopStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text)) => {
                texts.push(text.text);
            }
            LoopStreamItem::FinalResponse(final_response) => {
                final_text = Some(final_response.response().to_string());
            }
            _ => {}
        }
    }

    assert_eq!(texts, vec!["Hello ".to_string(), "world".to_string()]);
    assert_eq!(final_text.as_deref(), Some("Hello world"));
}
