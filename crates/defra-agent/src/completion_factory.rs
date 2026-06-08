use rig::agent::{Agent, AgentBuilder, PromptHook};
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use rig::message::ToolChoice;
use rig::tool::ToolDyn;

use crate::admission::{AdmissionRegistry, AdmittedCompletionClient};
use crate::backend_provider::BackendProviderKind;
use crate::config::{AgentBehavior, SamplingConfig};
use crate::watcher::AgentRequest;

fn effective_max_tokens(max_output_tokens: usize, sampling_max_tokens: Option<u64>) -> Option<u64> {
    sampling_max_tokens.or_else(|| u64::try_from(max_output_tokens).ok())
}

pub(crate) fn build_admitted_agent<C>(
    client: C,
    admission: AdmissionRegistry,
    behavior: &AgentBehavior,
    preamble: &str,
    tools: Vec<Box<dyn ToolDyn>>,
) -> Agent<<AdmittedCompletionClient<C> as CompletionClient>::CompletionModel>
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    let client = AdmittedCompletionClient::new(client, admission);
    let builder = configure_agent_builder(
        client
            .agent(&behavior.model_name)
            .preamble(preamble)
            .default_max_turns(behavior.max_turns),
        behavior,
        tools.len(),
    );

    if tools.is_empty() {
        builder.build()
    } else {
        builder.tools(tools).build()
    }
}

fn configure_agent_builder<M, P, ToolState>(
    mut builder: AgentBuilder<M, P, ToolState>,
    behavior: &AgentBehavior,
    tool_count: usize,
) -> AgentBuilder<M, P, ToolState>
where
    M: CompletionModel,
    P: PromptHook<M>,
{
    if tool_count > 0 {
        builder = builder.tool_choice(ToolChoice::Auto);
    }

    if let Some(temperature) = behavior.sampling.temperature {
        builder = builder.temperature(temperature);
    }

    if let Some(max_tokens) =
        effective_max_tokens(behavior.max_output_tokens, behavior.sampling.max_tokens)
    {
        builder = builder.max_tokens(max_tokens);
    }

    if let Some(additional_params) = merge_optional_params(
        provider_additional_params(behavior.backend_provider_kind),
        behavior.sampling.additional_params(),
    ) {
        builder = builder.additional_params(additional_params);
    }

    builder
}

/// Build a [`LoopConfig`](crate::agent::loop_stream::LoopConfig) for the owned
/// completion loop from a behavior, mirroring [`configure_agent_builder`]'s
/// resolution (tool choice when tools are present, sampling, provider params).
/// Used by call sites that drive the owned loop directly rather than via a rig
/// `Agent` (e.g. one-shot execution).
pub(crate) fn loop_config(
    behavior: &AgentBehavior,
    preamble: String,
    tool_count: usize,
) -> crate::agent::loop_stream::LoopConfig {
    crate::agent::loop_stream::LoopConfig {
        preamble: Some(preamble),
        temperature: behavior.sampling.temperature,
        max_tokens: effective_max_tokens(behavior.max_output_tokens, behavior.sampling.max_tokens),
        additional_params: merge_optional_params(
            provider_additional_params(behavior.backend_provider_kind),
            behavior.sampling.additional_params(),
        ),
        tool_choice: (tool_count > 0).then_some(ToolChoice::Auto),
        max_turns: behavior.max_turns,
    }
}

pub(crate) fn agent_with_request_sampling<M>(
    agent: &Agent<M>,
    behavior: &AgentBehavior,
    request: &AgentRequest,
) -> Agent<M>
where
    M: CompletionModel,
{
    let sampling = sampling_for_request(behavior.sampling, request);
    let mut agent = agent.clone();
    agent.temperature = sampling.temperature;
    agent.max_tokens = effective_max_tokens(behavior.max_output_tokens, sampling.max_tokens);
    let request_additional_params = merge_optional_params(
        sampling.additional_params(),
        request_additional_params(behavior, request),
    );
    if let Some(additional_params) = request_additional_params {
        agent.additional_params =
            merge_optional_params(agent.additional_params.take(), Some(additional_params));
    }
    agent
}

fn sampling_for_request(defaults: SamplingConfig, request: &AgentRequest) -> SamplingConfig {
    SamplingConfig {
        temperature: request.temperature.or(defaults.temperature),
        top_p: request.top_p.or(defaults.top_p),
        top_k: request.top_k.or(defaults.top_k),
        max_tokens: request
            .max_tokens
            .and_then(|value| u64::try_from(value).ok())
            .or(defaults.max_tokens),
    }
}

fn merge_optional_params(
    left: Option<serde_json::Value>,
    right: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    match (left, right) {
        (Some(left), Some(right)) => Some(merge_json_values(left, right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn merge_json_values(left: serde_json::Value, right: serde_json::Value) -> serde_json::Value {
    match (left, right) {
        (serde_json::Value::Object(mut left), serde_json::Value::Object(right)) => {
            for (key, right_value) in right {
                let value = left
                    .remove(&key)
                    .map(|left_value| merge_json_values(left_value, right_value.clone()))
                    .unwrap_or(right_value);
                left.insert(key, value);
            }
            serde_json::Value::Object(left)
        }
        (_, right) => right,
    }
}

fn provider_additional_params(kind: BackendProviderKind) -> Option<serde_json::Value> {
    match kind {
        BackendProviderKind::OpenAiCompatible => None,
        BackendProviderKind::OpenRouter => Some(
            rig::providers::openrouter::ProviderPreferences::new()
                .require_parameters(true)
                .to_json(),
        ),
        BackendProviderKind::ChatGptCodex => None,
    }
}

fn request_additional_params(
    behavior: &AgentBehavior,
    request: &AgentRequest,
) -> Option<serde_json::Value> {
    match behavior.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => openai_cache_scope_params(request),
        BackendProviderKind::OpenRouter => None,
        BackendProviderKind::ChatGptCodex => None,
    }
}

fn openai_cache_scope_params(request: &AgentRequest) -> Option<serde_json::Value> {
    let scope = normalize_cache_scope(request.session_id.as_str())
        .or_else(|| normalize_cache_scope(request.request_id.as_str()))?;
    Some(serde_json::json!({ "user": scope }))
}

fn normalize_cache_scope(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests;
