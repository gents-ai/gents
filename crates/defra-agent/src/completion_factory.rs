use crate::llm::ToolChoice;
use chrono::{DateTime, Utc};
use rig::client::CompletionClient;
use rig::completion::CompletionModel;

use crate::admission::{AdmissionRegistry, AdmittedCompletionClient};
use crate::agent::completion_retry::CompletionRetryPolicy;
use crate::agent::loop_stream::LoopConfig;
use crate::backend_provider::BackendProviderKind;
use crate::config::{AgentBehavior, SamplingConfig};
use crate::lifecycle::ExecutionOrigin;
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
        context_message: None,
        temperature: behavior.sampling.temperature,
        max_tokens: effective_max_tokens(behavior.max_output_tokens, behavior.sampling.max_tokens),
        // Thinking-on default is the leftmost base so that provider params,
        // behavior sampling, and per-request overrides all deep-merge ON TOP of
        // it (`merge_optional_params` lets the right-hand side win key-by-key).
        // A caller that explicitly sets `chat_template_kwargs.enable_thinking`
        // therefore overrides this default rather than being clobbered by it.
        additional_params: merge_optional_params(
            merge_optional_params(
                thinking_default_params(),
                provider_additional_params(behavior.backend_provider_kind),
            ),
            behavior.sampling.additional_params(),
        ),
        tool_choice: (tool_count > 0).then_some(ToolChoice::Auto),
        on_rendered_request: None,
        retry_policy: CompletionRetryPolicy::scheduled_default(),
        deadline: None,
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
    let origin = completion_retry_origin(request.execution_origin.as_deref());
    config.retry_policy = CompletionRetryPolicy::resolve(&behavior.completion_retry, origin);
    config.deadline = parse_request_deadline(request.deadline.as_deref());
    config
}

fn completion_retry_origin(value: Option<&str>) -> ExecutionOrigin {
    match value {
        Some("interactive") => ExecutionOrigin::Interactive,
        _ => ExecutionOrigin::Scheduled,
    }
}

fn parse_request_deadline(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value.trim()).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn sampling_for_request(defaults: SamplingConfig, request: &AgentRequest) -> SamplingConfig {
    SamplingConfig {
        temperature: request.temperature.or(defaults.temperature),
        top_p: request.top_p.or(defaults.top_p),
        top_k: request.top_k.or(defaults.top_k),
        // No per-request override exists for these today; the profile's value
        // (or the served model's default, when unset) stands.
        min_p: defaults.min_p,
        frequency_penalty: defaults.frequency_penalty,
        presence_penalty: defaults.presence_penalty,
        repetition_penalty: defaults.repetition_penalty,
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

/// Default request params that turn the model's reasoning/thinking trace ON.
///
/// vLLM's OpenAI-compatible server (with a `--reasoning-parser`, e.g.
/// `deepseek_v4` on the d4f harvest server) only emits the chain-of-thought in
/// the response `message.reasoning` field when the request carries
/// `chat_template_kwargs={"enable_thinking": true}`. Without it the server
/// defaults thinking OFF and the `reasoning` field is empty, so our harvest
/// trajectories lose the model's reasoning. We default this ON for every
/// outbound completion request; because it is merged as the leftmost base in
/// [`loop_config`], any caller-supplied `chat_template_kwargs` deep-merges over
/// it and a caller can flip `enable_thinking` back to `false` if desired.
///
/// The key is serialized flat into the OpenAI completion body (rig flattens
/// `additional_params`), so it reaches vLLM as a top-level `chat_template_kwargs`
/// object — exactly where the server reads it.
fn thinking_default_params() -> Option<serde_json::Value> {
    Some(serde_json::json!({
        // Responses API (our default for OpenAI-compatible + ChatGptCodex):
        // structured reasoning, controlled by `reasoning.effort`. This is the
        // first-class, round-trippable reasoning surface. TODO(inference-profile):
        // thread the effort level (low/medium/high) from behavior config instead
        // of defaulting to medium.
        "reasoning": { "effort": "medium" },
        // Chat Completions fallback (CompletionsClient / vLLM `--reasoning-parser`):
        // the legacy thinking toggle. Ignored by the Responses endpoint.
        "chat_template_kwargs": { "enable_thinking": true }
    }))
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
