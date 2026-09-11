use crate::llm::ToolChoice;
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use rig::client::CompletionClient;
use rig::completion::CompletionModel;
use serde::Deserialize;

use crate::admission::{AdmissionRegistry, AdmittedCompletionClient};
use crate::agent::completion_retry::CompletionRetryPolicy;
use crate::agent::loop_stream::{AggregateTokenBudget, LoopConfig};
use crate::backend_provider::BackendProviderKind;
use crate::config::{ReasoningEffort, ResolvedBehavior};
use crate::graphql::escape_graphql_string;
use crate::lifecycle::ExecutionOrigin;
use crate::openai_wire::OpenAiWireApi;
use crate::provider_usage;
use crate::rendered_request::CaptureScopeKind;
use crate::watcher::AgentRequest;

fn effective_max_tokens(max_output_tokens: usize, sampling_max_tokens: Option<u64>) -> Option<u64> {
    sampling_max_tokens.or_else(|| u64::try_from(max_output_tokens).ok())
}

pub(crate) fn build_admitted_model<C>(
    client: C,
    admission: AdmissionRegistry,
    behavior: &ResolvedBehavior,
) -> <AdmittedCompletionClient<C> as CompletionClient>::CompletionModel
where
    C: CompletionClient,
    C::CompletionModel: 'static,
    <C::CompletionModel as CompletionModel>::Response: 'static,
    <C::CompletionModel as CompletionModel>::StreamingResponse: 'static,
{
    AdmittedCompletionClient::new(client, admission).completion_model(&behavior.model_name)
}

/// Build a loop config for one completion loop.
///
/// `capture_scope` is not decoration. Every loop this factory serves issues
/// provider calls under the same `(agent_did, session_id, request_id)`, and
/// every one of them starts its turn and attempt counters at zero — the owned
/// inference loop, the compaction summarizer and its JSON fallback, title
/// generation, and the one-shot runner. The scope is what keeps their first
/// calls from colliding into one durable fact, and passing it here is what
/// makes capture the default for all of them instead of a privilege of the
/// inference path (#840).
pub(crate) fn loop_config(
    behavior: &ResolvedBehavior,
    preamble: String,
    tool_count: usize,
    capture_scope: CaptureScopeKind,
) -> LoopConfig {
    LoopConfig {
        provider_input_counter: std::sync::Arc::new(
            crate::provider_input::ProviderInputCounter::new(
                behavior.backend_provider_kind,
                behavior.openai_wire_api,
                behavior.model_name.clone(),
            ),
        ),
        preamble: Some(preamble),
        context_message: None,
        temperature: behavior.sampling.temperature,
        max_tokens: effective_max_tokens(behavior.max_output_tokens, behavior.sampling.max_tokens),
        aggregate_token_budget: None,
        additional_params: merge_optional_params(
            merge_optional_params(
                reasoning_profile_params(
                    behavior.backend_provider_kind,
                    behavior.openai_wire_api,
                    behavior.sampling.reasoning_effort,
                ),
                provider_additional_params(behavior.backend_provider_kind),
            ),
            behavior.sampling.additional_params(),
        ),
        structured_output: None,
        tool_choice: (tool_count > 0).then_some(ToolChoice::Auto),
        on_rendered_request: Some(crate::rendered_request::scope::ambient_arming_sink(
            capture_scope,
        )),
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        context_window: behavior.context_window,
        compaction_threshold: behavior.compaction_threshold(),
        retry_policy: CompletionRetryPolicy::scheduled_default(),
        deadline: None,
        max_turns: behavior.max_turns,
        output_obligation_gate: None,
    }
}

pub(crate) fn loop_config_for_request(
    behavior: &ResolvedBehavior,
    preamble: String,
    request: &AgentRequest,
    aggregate_token_budget: Option<AggregateTokenBudget>,
    tool_count: usize,
) -> anyhow::Result<LoopConfig> {
    let mut config = loop_config(behavior, preamble, tool_count, CaptureScopeKind::Inference);
    behavior
        .sampling
        .validate_for_provider(behavior.backend_provider_kind, behavior.openai_wire_api)?;
    config.aggregate_token_budget = aggregate_token_budget;
    let request_additional_params = request_additional_params(behavior, request);
    if let Some(additional_params) = request_additional_params {
        config.additional_params =
            merge_optional_params(config.additional_params.take(), Some(additional_params));
    }
    let origin = ExecutionOrigin::from_persisted(request.execution_origin.as_deref())?;
    config.retry_policy = CompletionRetryPolicy::resolve(&behavior.completion_retry, origin);
    config.deadline = parse_request_deadline(request.deadline.as_deref());
    Ok(config)
}

/// Decode the execution owner's pinned request limit. Zero is a valid
/// exhausted durable state; configured limits are validated positive before claim.
pub(crate) fn parse_aggregate_token_limit(
    max_total_tokens: Option<i64>,
) -> anyhow::Result<Option<u64>> {
    max_total_tokens
        .map(|limit| {
            let limit = u64::try_from(limit)
                .map_err(|_| anyhow::anyhow!("pinned max_total_tokens must not be negative"))?;
            Ok(limit)
        })
        .transpose()
}

/// Mint the single monotone provider-usage ledger for one durable request.
///
/// `used` is rehydrated from durable `InferenceCall` rows for this physical
/// `request_doc_id` so crash redrive and process restart cannot mint a fresh
/// allowance. Callers must construct it before any request-scoped provider
/// work so session compaction and the owned inference loop share one handle.
pub(crate) async fn aggregate_token_budget_for_request(
    node: &EmbeddedNode,
    request: &AgentRequest,
) -> anyhow::Result<Option<AggregateTokenBudget>> {
    let Some(limit) = parse_aggregate_token_limit(request.max_total_tokens)? else {
        return Ok(None);
    };
    let used = load_prior_charged_tokens(node, &request.doc_id).await?;
    if used == 0 {
        return Ok(Some(AggregateTokenBudget::new(limit)));
    }
    tracing::info!(
        request_id = %request.request_id,
        request_doc_id = %request.doc_id,
        used,
        limit,
        "rehydrated aggregate token budget from durable InferenceCall rows"
    );
    Ok(Some(AggregateTokenBudget::with_prior_usage(limit, used)))
}

#[derive(Debug, Deserialize)]
struct PriorUsageRow {
    prompt_tokens: Option<i64>,
    completion_tokens: Option<i64>,
}

/// Sum charged tokens already persisted for this physical request.
async fn load_prior_charged_tokens(
    node: &EmbeddedNode,
    request_doc_id: &str,
) -> anyhow::Result<u64> {
    anyhow::ensure!(
        !request_doc_id.trim().is_empty(),
        "budget rehydration requires the physical request document ID"
    );
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ request_doc_id: {{ _eq: "{}" }} }}
            ) {{
                prompt_tokens
                completion_tokens
            }}
        }}"#,
        escape_graphql_string(request_doc_id)
    );
    let resp = node.execute(&query).await;
    if resp.has_errors() {
        anyhow::bail!(
            "loading prior InferenceCall usage for request_doc_id={request_doc_id}: {:?}",
            resp.errors
        );
    }
    let rows = prior_usage_rows_from_response(
        resp.data
            .as_ref()
            .and_then(|data| data.get("InferenceCall")),
    )
    .map_err(|error| {
        anyhow::anyhow!("decoding InferenceCall usage for request_doc_id={request_doc_id}: {error}")
    })?;
    provider_usage::sum_charged_from_persisted_parts(
        rows.into_iter()
            .map(|row| (row.prompt_tokens, row.completion_tokens)),
    )
    .map_err(|error| {
        anyhow::anyhow!("decoding InferenceCall usage for request_doc_id={request_doc_id}: {error}")
    })
}

/// Decode the `InferenceCall` array from a GraphQL data payload.
/// Missing/null → empty (no prior usage). Present but malformed → error.
fn prior_usage_rows_from_response(
    value: Option<&serde_json::Value>,
) -> anyhow::Result<Vec<PriorUsageRow>> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            anyhow::anyhow!("InferenceCall usage payload is not a row array: {error}")
        }),
    }
}

fn parse_request_deadline(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|value| DateTime::parse_from_rfc3339(value.trim()).ok())
        .map(|value| value.with_timezone(&Utc))
}

// Moved to gents-loop (G-1): compaction's per-turn summary request also
// merges additional_params, with no dependency on AgentBehavior otherwise.
pub(crate) use gents_loop::compaction::merge_optional_params;

/// Maps the inference profile's reasoning effort into each provider's wire
/// contract. An absent profile setting injects no reasoning default, except for
/// ChatGPT Codex: it has a known Responses contract and keeps the `medium`
/// default it shipped before reasoning effort became profile configuration
/// (#540).
///
/// vLLM's OpenAI-compatible server (with a `--reasoning-parser`, e.g.
/// `deepseek_v4` on the d4f harvest server) only emits the chain-of-thought in
/// the response `message.reasoning` field when the request carries
/// `chat_template_kwargs={"enable_thinking": true}`. Without it the server
/// defaults thinking OFF and the `reasoning` field is empty, so our harvest
/// trajectories lose the model's reasoning. That local-only toggle must not be
/// sent to Responses or OpenRouter endpoints, which use the standard
/// `reasoning.effort` object instead.
///
/// The key is serialized flat into the OpenAI completion body (rig flattens
/// `additional_params`), so it reaches vLLM as a top-level `chat_template_kwargs`
/// object — exactly where the server reads it.
fn reasoning_profile_params(
    kind: BackendProviderKind,
    wire_api: OpenAiWireApi,
    reasoning_effort: Option<ReasoningEffort>,
) -> Option<serde_json::Value> {
    let Some(reasoning_effort) = reasoning_effort else {
        // Codex predates profile-configured reasoning: it has a known Responses
        // contract and shipped an unconditional `medium`. Profile plumbing may
        // override that default, never silently drop it (#540). Every other
        // backend waits for explicit configuration.
        return matches!(kind, BackendProviderKind::ChatGptCodex).then(|| {
            serde_json::json!({
                "reasoning": { "effort": ReasoningEffort::Medium.as_str() }
            })
        });
    };
    match (kind, wire_api) {
        (BackendProviderKind::OpenAiCompatible, OpenAiWireApi::ChatCompletions) => {
            let mut kwargs = serde_json::Map::from_iter([(
                "enable_thinking".to_string(),
                serde_json::Value::Bool(reasoning_effort != ReasoningEffort::None),
            )]);
            if reasoning_effort != ReasoningEffort::None {
                kwargs.insert(
                    "reasoning_effort".to_string(),
                    serde_json::Value::String(reasoning_effort.as_str().to_string()),
                );
            }
            Some(serde_json::json!({ "chat_template_kwargs": kwargs }))
        }
        (BackendProviderKind::OpenAiCompatible, OpenAiWireApi::Responses)
        | (BackendProviderKind::OpenRouter, _)
        | (BackendProviderKind::ChatGptCodex, _) => Some(serde_json::json!({
            "reasoning": { "effort": reasoning_effort.as_str() }
        })),
        // Grok: do not force reasoning.effort — several grok models 400 on it.
        (BackendProviderKind::XaiGrokOAuth, _) => None,
        // Claude Messages sends no sampling or reasoning keys (`claude_messages`).
        (BackendProviderKind::ClaudeCliSubscription, _) => None,
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
        BackendProviderKind::ChatGptCodex
        | BackendProviderKind::XaiGrokOAuth
        | BackendProviderKind::ClaudeCliSubscription => None,
    }
}

fn request_additional_params(
    behavior: &ResolvedBehavior,
    request: &AgentRequest,
) -> Option<serde_json::Value> {
    match behavior.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => openai_cache_scope_params(request),
        BackendProviderKind::OpenRouter
        | BackendProviderKind::ChatGptCodex
        | BackendProviderKind::XaiGrokOAuth
        | BackendProviderKind::ClaudeCliSubscription => None,
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

/// Optional summary provider, built by the same client/admission factory as
/// ordinary inference. The enclosing request later supplies deadline and ledger.
pub(crate) async fn build_compaction_engine(
    node: std::sync::Arc<EmbeddedNode>,
    behavior: &ResolvedBehavior,
    admission: AdmissionRegistry,
    build_timeout: std::time::Duration,
) -> anyhow::Result<Option<std::sync::Arc<dyn crate::compaction::ReductionEngine>>> {
    let Some(inference) = &behavior.compaction_inference else {
        anyhow::ensure!(
            matches!(
                behavior.compaction_strategy().reduction_mode(),
                crate::compaction::ReductionMode::StripOnly
            ) || behavior
                .compaction
                .as_ref()
                .and_then(|config| config.inference_profile_id.as_ref())
                .is_none(),
            "selected compaction inference profile was not resolved"
        );
        return Ok(None);
    };
    if matches!(
        behavior.compaction_strategy().reduction_mode(),
        crate::compaction::ReductionMode::StripOnly
    ) {
        return Ok(None);
    }
    anyhow::ensure!(
        inference.backend.agent_did == behavior.agent_did()
            && inference.profile.agent_did == behavior.agent_did(),
        "summary inference must belong to the invoking principal"
    );
    anyhow::ensure!(
        inference.profile.backend_id == inference.backend.backend_id,
        "summary profile/backend reference mismatch"
    );
    inference.backend.validate()?;
    // This is the existing resolved runtime view, not another authored config.
    let mut summary = behavior.clone();
    let backend = inference.backend.backend_fields();
    summary.backend_id = backend.backend_id;
    summary.backend_provider_kind = backend.backend_provider_kind;
    summary.openai_wire_api = backend.openai_wire_api;
    summary.backend_endpoint = backend.backend_endpoint;
    summary.backend_auth = backend.backend_auth;
    summary.model_name = inference.profile.model_name.clone();
    summary.context_window = inference.context_window()?;
    summary.max_output_tokens = inference.max_output_tokens()?;
    summary.sampling = inference.sampling_config()?;
    summary.max_turns = 0;
    summary.compaction_inference = None;
    let api_key = match &summary.backend_auth {
        crate::document_config::BackendAuth::PrincipalOAuth => "no-key".to_owned(),
        _ => summary.completion_client_api_key()?,
    };
    let client =
        crate::llm::backend_client::build_backend_client(node, &summary, &api_key, build_timeout)
            .await?;
    let source_counter = std::sync::Arc::new(crate::provider_input::ProviderInputCounter::new(
        behavior.backend_provider_kind,
        behavior.openai_wire_api,
        behavior.model_name.clone(),
    ));
    let config = loop_config(&summary, String::new(), 0, CaptureScopeKind::Compaction);
    let backend_id = inference.backend.backend_id.clone();
    let engine = crate::llm::backend_client::with_backend_client!(client, |client| {
        let model = std::sync::Arc::new(build_admitted_model(client, admission, &summary));
        crate::compaction::backend_scoped_reduction_engine(
            std::sync::Arc::new(
                crate::compaction::ProviderReductionEngine::new(model, config)
                    .with_source_input_counter(source_counter)
                    .with_summary_output_limit(inference.max_output_tokens()?),
            ),
            backend_id,
        ) as std::sync::Arc<dyn crate::compaction::ReductionEngine>
    });
    Ok(Some(engine))
}
