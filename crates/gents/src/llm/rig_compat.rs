pub use gents_loop::rig_compat::*;

use async_stream::try_stream;
use futures::StreamExt;
use rig::client::CompletionClient;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, GetTokenUsage, Usage,
};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamingCompletionResponse,
};

use crate::admission::stream_guard::{finish_guard, StreamGuardLifecycle};
use crate::admission::{
    AdmissionError, AdmissionRegistry, AdmittedCallError, AdmittedCompletionModel,
    ProviderCallUsage,
};

impl From<Usage> for ProviderCallUsage {
    fn from(usage: Usage) -> Self {
        Self(gents_protocol::usage::TokenUsage {
            input_tokens: Some(usage.input_tokens),
            output_tokens: Some(usage.output_tokens),
            cached_input_tokens: (usage.cached_input_tokens > 0)
                .then_some(usage.cached_input_tokens),
            cache_creation_input_tokens: (usage.cache_creation_input_tokens > 0)
                .then_some(usage.cache_creation_input_tokens),
            final_response: true,
            ..Default::default()
        })
    }
}

impl From<AdmissionError> for CompletionError {
    fn from(error: AdmissionError) -> Self {
        CompletionError::ProviderError(error.0)
    }
}

fn admitted_call_error(error: AdmittedCallError<CompletionError>) -> CompletionError {
    match error {
        AdmittedCallError::Admission(error) => error.into(),
        AdmittedCallError::Provider(error) => error,
    }
}

#[derive(Clone)]
pub(crate) struct AdmittedCompletionClient<C> {
    inner: C,
    admission: AdmissionRegistry,
    connection: std::sync::Arc<str>,
    profile_id: Option<String>,
}

impl<C> CompletionClient for AdmittedCompletionClient<C>
where
    C: ProviderClient,
{
    type CompletionModel = AdmittedCompletionModel<C::CompletionModel>;
}

pub(crate) fn admitted_model<C>(
    client: C,
    admission: AdmissionRegistry,
    connection: String,
    model_name: &str,
    profile_id: Option<&str>,
) -> AdmittedCompletionModel<C::CompletionModel>
where
    C: ProviderClient,
{
    AdmittedCompletionClient {
        inner: client,
        admission,
        connection: connection.into(),
        profile_id: profile_id.map(str::to_owned),
    }
    .completion_model(model_name)
}

impl<M> CompletionModel for AdmittedCompletionModel<M>
where
    M: ProviderModel,
{
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = AdmittedCompletionClient<M::Client>;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        let model = model.into();
        let mut admitted = AdmittedCompletionModel::new(
            M::make(&client.inner, model.clone()),
            client.admission.clone(),
            client.connection.clone(),
        );
        admitted.model_name = Some(model);
        admitted.profile_id = client.profile_id.clone();
        admitted
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let (response, mut permit) = self
            .admit(false, self.inner.completion(request))
            .await
            .map_err(admitted_call_error)?;
        let cached = cached_input_tokens_observation(&response.raw_response);
        permit
            .finish_success_with_cache(
                Some(ProviderCallUsage(observed_token_usage(
                    &response.raw_response,
                    Some(response.usage),
                ))),
                cached,
            )
            .await?;
        Ok(response)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let (stream, permit) = self
            .admit(true, self.inner.stream(request))
            .await
            .map_err(admitted_call_error)?;
        Ok(hold_stream_guard(stream, permit))
    }
}

pub(crate) fn hold_stream_guard<R, G>(
    stream: StreamingCompletionResponse<R>,
    guard: G,
) -> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage + serde::Serialize + Send + 'static,
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
                        terminal_guard.mark_stream_success(
                            Some(ProviderCallUsage(observed_token_usage(response, response.token_usage()))),
                            cached_input_tokens_observation(response),
                        );
                        // The owned loop charges from the terminal item, while
                        // crash rehydrate charges from the InferenceCall row:
                        // persist before publishing so the two cannot diverge.
                        finish_guard(terminal_guard).await?;
                    }
                    for choice in streamed_item_to_raw_choices(item) {
                        yield choice;
                    }
                }
                Err(error) => {
                    if let Some(mut terminal_guard) = guard.take() {
                        terminal_guard.mark_stream_error(&error.to_string());
                        finish_guard(terminal_guard).await?;
                    }
                    Err(error)?;
                }
            }
        }
        if let Some(mut terminal_guard) = guard.take() {
            terminal_guard.mark_stream_success(None, CachedInputTokensObservation::NotAvailable);
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
