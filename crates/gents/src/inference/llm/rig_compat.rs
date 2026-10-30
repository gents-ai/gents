pub use gents_loop::rig_compat::*;

use async_stream::try_stream;
use futures::{FutureExt, StreamExt};
use rig::client::CompletionClient;
use rig::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, GetTokenUsage, Usage,
};
use rig::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent, StreamingCompletionResponse,
};

use crate::inference::admission::stream_guard::{finish_guard, StreamGuardLifecycle};
use crate::inference::admission::{
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
    separate_reasoning: bool,
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
    separate_reasoning: bool,
) -> AdmittedCompletionModel<C::CompletionModel>
where
    C: ProviderClient,
{
    AdmittedCompletionClient {
        inner: client,
        admission,
        connection: connection.into(),
        profile_id: profile_id.map(str::to_owned),
        separate_reasoning,
    }
    .completion_model(model_name)
}

impl<M> CompletionModel for AdmittedCompletionModel<M>
where
    M: ProviderModel,
{
    type Response = M::Response;
    type StreamingResponse = ObservedStreamingResponse<M::StreamingResponse>;
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
        admitted.separate_reasoning = client.separate_reasoning;
        admitted
    }

    async fn completion(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        let (mut response, mut permit) = self
            .admit(false, self.inner.completion(request))
            .await
            .map_err(admitted_call_error)?;
        let cached = cached_input_tokens_observation(&response.raw_response);
        let observed = call_usage(&response.raw_response, self.separate_reasoning);
        response.usage = compat_usage(observed).unwrap_or_default();
        permit
            .finish_success_with_cache(Some(ProviderCallUsage(observed)), cached)
            .await?;
        Ok(response)
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let (stream, permit) = self
            .admit(true, self.inner.stream(request).boxed())
            .await
            .map_err(admitted_call_error)?;
        let separate = self.separate_reasoning;
        Ok(hold_stream_guard_mapped(
            stream,
            permit,
            separate,
            move |raw| ObservedStreamingResponse::new(raw, separate),
        ))
    }
}

/// Budget counters and durable admission counters must come from the same
/// observation, while serialization retains the provider's original payload.
/// Deserializing raw transport data cannot recover the selected provider
/// dialect, so it leaves the derived budget projection unknown.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct ObservedStreamingResponse<R> {
    #[serde(flatten)]
    raw: R,
    #[serde(skip)]
    usage: Option<Usage>,
}

fn call_usage<R>(raw: &R, separate: bool) -> gents_protocol::usage::TokenUsage
where
    R: serde::Serialize,
{
    let usage = observed_token_usage(raw);
    if separate {
        usage.with_separate_reasoning()
    } else {
        usage
    }
}

/// Rig requires integer subdivisions. Their defaults stay in this compatibility
/// projection and cannot establish evidence in the durable usage ledger.
fn compat_usage(observed: gents_protocol::usage::TokenUsage) -> Option<Usage> {
    let total_tokens = observed.charged_tokens().ok().flatten()?;
    Some(Usage {
        input_tokens: observed.input_tokens?,
        output_tokens: observed.output_tokens?,
        total_tokens,
        cached_input_tokens: observed.cached_input_tokens.unwrap_or(0),
        cache_creation_input_tokens: observed.cache_creation_input_tokens.unwrap_or(0),
    })
}

impl<R> ObservedStreamingResponse<R>
where
    R: serde::Serialize,
{
    fn new(raw: R, separate: bool) -> Self {
        let usage = compat_usage(call_usage(&raw, separate));
        Self { raw, usage }
    }
}

impl<R> GetTokenUsage for ObservedStreamingResponse<R> {
    fn token_usage(&self) -> Option<Usage> {
        self.usage
    }
}

#[cfg(test)]
pub(crate) fn hold_stream_guard<R, G>(
    stream: StreamingCompletionResponse<R>,
    guard: G,
) -> StreamingCompletionResponse<R>
where
    R: Clone + Unpin + GetTokenUsage + serde::Serialize + Send + 'static,
    G: StreamGuardLifecycle + Send + Unpin + 'static,
{
    hold_stream_guard_mapped(stream, guard, false, |raw| raw)
}

fn hold_stream_guard_mapped<R, G, S, F>(
    stream: StreamingCompletionResponse<R>,
    guard: G,
    separate_reasoning: bool,
    map: F,
) -> StreamingCompletionResponse<S>
where
    R: Clone + Unpin + GetTokenUsage + serde::Serialize + Send + 'static,
    S: Clone + Unpin + GetTokenUsage + serde::Serialize + Send + 'static,
    G: StreamGuardLifecycle + Send + Unpin + 'static,
    F: Fn(R) -> S + Send + 'static,
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
                            Some(ProviderCallUsage(call_usage(response, separate_reasoning))),
                            cached_input_tokens_observation(response),
                        );
                        // The owned loop charges from the terminal item, while
                        // crash rehydrate charges from the InferenceCall row:
                        // persist before publishing so the two cannot diverge.
                        finish_guard(terminal_guard).await?;
                    }
                    for choice in streamed_item_to_raw_choices(item, &map) {
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

fn streamed_item_to_raw_choices<R, S>(
    item: StreamedAssistantContent<R>,
    map: impl Fn(R) -> S,
) -> Vec<RawStreamingChoice<S>>
where
    R: Clone,
    S: Clone,
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
            vec![RawStreamingChoice::FinalResponse(map(response))]
        }
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    #[test]
    fn xai_chat_live_and_durable_usage_share_the_executable_normalization() {
        let snapshot: serde_json::Value =
            serde_json::from_str(&gents_lean_contract::load_contract_json().unwrap()).unwrap();
        let case = snapshot["provider_usage_cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["name"] == "wire_xai_separate_reasoning")
            .unwrap();
        let expected: gents_protocol::usage::TokenUsage =
            serde_json::from_value(case["expected"][0]["separate_reasoning"].clone()).unwrap();
        let raw: rig::providers::openai::completion::streaming::StreamingCompletionResponse =
            serde_json::from_value(serde_json::json!({"usage":{
                "prompt_tokens":32,"completion_tokens":9,"total_tokens":151,
                "prompt_tokens_details":{"cached_tokens":8},
                "completion_tokens_details":{"reasoning_tokens":110}
            }}))
            .unwrap();
        let original = serde_json::to_value(&raw).unwrap();
        let durable = call_usage(&raw, true);
        let wrapped = ObservedStreamingResponse::new(raw, true);
        assert_eq!(durable, expected);
        let live = wrapped.token_usage().unwrap();
        assert_eq!(Some(live.total_tokens), expected.charged_tokens().unwrap());
        assert_eq!(Some(live.input_tokens), expected.input_tokens);
        assert_eq!(Some(live.output_tokens), expected.output_tokens);
        assert_eq!(serde_json::to_value(wrapped).unwrap(), original);
    }

    #[test]
    fn live_usage_does_not_charge_missing_or_invalid_raw_counters() {
        for raw in [
            serde_json::json!({"usage":null}),
            serde_json::json!({"usage":{"input_tokens":10,"output_tokens":5,"input_tokens_details":{"cached_tokens":11}}}),
        ] {
            assert!(ObservedStreamingResponse::new(raw, false)
                .token_usage()
                .is_none());
        }
    }
}
