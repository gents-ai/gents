use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use crate::llm::ToolChoice;

use crate::admission::{AdmissionRegistry, AdmittedCompletionClient};
use crate::agent::loop_stream::LoopConfig;
use crate::backend_provider::BackendProviderKind;
use crate::config::{AgentBehavior, SamplingConfig};
use crate::watcher::AgentRequest;

fn effective_max_tokens(max_output_tokens: usize, sampling_max_tokens: Option<u64>) -> Option<u64> {
    sampling_max_tokens.or_else(|| u64::try_from(max_output_tokens).ok())
}

/// Build the admission-wrapped completion model for a behavior. The owned loop
/// (#400) drives this model directly — there is no rig `Agent`; per-request
/// configuration is produced separately by [`loop_config`] / [`loop_config_for_request`].
pub(crate) fn build_admitted_model<C>(
    client: C,
    admission: AdmissionRegistry,
    behavior: &AgentBehavior,
) -> <AdmittedCompletionClient<C> as CompletionClient>::CompletionModel
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    AdmittedCompletionClient::new(client, admission).completion_model(&behavior.model_name)
}

/// Build a [`LoopConfig`] from a behavior: tool choice when tools are present,
/// behavior-level sampling, provider params, and the behavior turn cap. This is
/// the base config (no per-request overrides).
pub(crate) fn loop_config(
    behavior: &AgentBehavior,
    preamble: String,
    tool_count: usize,
) -> LoopConfig {
    LoopConfig {
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

/// Base [`loop_config`] with per-request sampling overrides applied — the
/// owned-loop replacement for the old `agent_with_request_sampling`. Request
/// temperature/max_tokens replace the defaults; request additional params merge
/// on top of the behavior/provider params.
pub(crate) fn loop_config_for_request(
    behavior: &AgentBehavior,
    preamble: String,
    request: &AgentRequest,
    tool_count: usize,
) -> LoopConfig {
    let mut config = loop_config(behavior, preamble, tool_count);
    let sampling = sampling_for_request(behavior.sampling, request);
    config.temperature = sampling.temperature;
    config.max_tokens = effective_max_tokens(behavior.max_output_tokens, sampling.max_tokens);
    let request_additional_params = merge_optional_params(
        sampling.additional_params(),
        request_additional_params(behavior, request),
    );
    if let Some(additional_params) = request_additional_params {
        config.additional_params =
            merge_optional_params(config.additional_params.take(), Some(additional_params));
    }
    config
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
