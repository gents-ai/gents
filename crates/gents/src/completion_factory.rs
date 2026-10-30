use crate::inference::llm::ToolChoice;
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde::Deserialize;

use crate::agent::completion_retry::CompletionRetryPolicy;
use crate::agent::loop_stream::{AggregateTokenBudget, LoopConfig};
use crate::graphql::escape_graphql_string;
use crate::inference::admission::{AdmissionRegistry, AdmittedCompletionModel};
use crate::inference::backends::provider::BackendProviderKind;
use crate::inference::rendered_request::CaptureScopeKind;
use crate::inference::resolved::{ReasoningEffort, ResolvedAgent};
use crate::lifecycle::ExecutionOrigin;
use crate::openai_wire::OpenAiWireApi;
use crate::provider_usage;
use crate::watcher::AgentRequest;

pub(crate) fn effective_max_tokens(
    max_output_tokens: usize,
    sampling_max_tokens: Option<u64>,
) -> Option<u64> {
    sampling_max_tokens.or_else(|| u64::try_from(max_output_tokens).ok())
}

pub(crate) fn agent_connection_fingerprint(agent_config: &ResolvedAgent) -> String {
    crate::inference::admission::backend_connection_fingerprint(
        &crate::inference::backends::registry::BackendFields {
            backend_id: agent_config.backend_id.clone(),
            backend_provider_kind: agent_config.backend_provider_kind,
            openai_wire_api: agent_config.openai_wire_api,
            backend_endpoint: agent_config.backend_endpoint.clone(),
            backend_auth: agent_config.backend_auth.clone(),
        },
    )
}

/// Identity of everything an agent_config slot builds once and keeps: its resolved
/// configuration plus the keyed connection identity of its inference and
/// compaction clients. `ResolvedAgent`'s Debug redacts credentials, so a
/// key rotation is visible only through the keyed fingerprints.
pub(crate) fn agent_slot_fingerprint(agent_config: &ResolvedAgent) -> String {
    let compaction = agent_config.compaction_inference.as_ref().map(|inference| {
        crate::inference::admission::backend_connection_fingerprint(
            &inference.backend.backend_fields(),
        )
    });
    format!(
        "{agent_config:?}|connection={}|compaction_connection={compaction:?}",
        agent_connection_fingerprint(agent_config)
    )
}

pub(crate) fn build_admitted_model<C>(
    client: C,
    admission: AdmissionRegistry,
    agent_config: &ResolvedAgent,
) -> AdmittedCompletionModel<C::CompletionModel>
where
    C: crate::inference::llm::rig_compat::ProviderClient,
{
    crate::inference::llm::rig_compat::admitted_model(
        client,
        admission,
        agent_connection_fingerprint(agent_config),
        &agent_config.model_name,
        agent_config.profile_id.as_deref(),
        agent_config.openai_wire_api == OpenAiWireApi::ChatCompletions
            && (agent_config.backend_provider_kind == BackendProviderKind::XaiGrokOAuth
                || (agent_config.backend_provider_kind == BackendProviderKind::OpenAiCompatible
                    && crate::inference::backends::setup::is_xai_api_endpoint(
                        &agent_config.backend_endpoint,
                    ))),
    )
}

pub(crate) fn loop_config(
    agent_config: &ResolvedAgent,
    preamble: String,
    tool_count: usize,
    capture_scope: CaptureScopeKind,
) -> LoopConfig {
    let provider_input_counter = std::sync::Arc::new(
        crate::provider_input::ProviderInputCounter::new(
            agent_config.backend_provider_kind,
            agent_config.openai_wire_api,
            agent_config.model_name.clone(),
        )
        .with_service_tier(agent_config.service_tier),
    );
    let context_calibration = crate::provider_input::budget::ContextCalibration::prior_for(
        provider_input_counter.profile(),
    );
    LoopConfig {
        replay: Default::default(),
        provider_input_counter,
        preamble: Some(preamble),
        context_message: None,
        temperature: agent_config.sampling.temperature,
        max_tokens: effective_max_tokens(
            agent_config.max_output_tokens,
            agent_config.sampling.max_tokens,
        ),
        aggregate_token_budget: None,
        additional_params: authoritative_reasoning_capabilities(
            merge_optional_params(
                merge_optional_params(
                    reasoning_profile_params(
                        agent_config.backend_provider_kind,
                        agent_config.openai_wire_api,
                        crate::inference::backends::setup::sent_reasoning_effort(
                            agent_config.backend_provider_kind,
                            agent_config.openai_wire_api,
                            &agent_config.backend_endpoint,
                            agent_config.resolved_reasoning_efforts.as_deref(),
                            agent_config.sampling.reasoning_effort,
                        ),
                    ),
                    provider_additional_params(
                        agent_config.backend_provider_kind,
                        agent_config.openai_wire_api,
                        &agent_config.backend_endpoint,
                    ),
                ),
                merge_optional_params(
                    agent_config.sampling.additional_params(),
                    agent_config
                        .service_tier
                        .filter(|_| {
                            agent_config.openai_wire_api == crate::OpenAiWireApi::ChatCompletions
                        })
                        .map(|tier| serde_json::json!({"service_tier":tier.wire_value()})),
                ),
            ),
            agent_config.backend_provider_kind,
            agent_config.resolved_reasoning_efforts.as_deref(),
        ),
        structured_output: None,
        tool_choice: (tool_count > 0).then_some(ToolChoice::Auto),
        on_rendered_request: Some(
            crate::inference::rendered_request::scope::ambient_arming_sink(capture_scope),
        ),
        turn_compactor: None,
        active_reduction_keys: Vec::new(),
        reduction_chain_keys: Vec::new(),
        initial_turn_index: 0,
        resume_from_history: false,
        context_window: agent_config.context_window,
        compaction_threshold: agent_config.compaction_threshold(),
        context_calibration,
        retry_policy: CompletionRetryPolicy::scheduled_default(),
        provider_idle_timeout: Some(agent_config.provider_idle_timeout),
        deadline: None,
        max_turns: agent_config.max_turns,
        output_obligation_gate: None,
        folded_prompts: Vec::new(),
        authored_input: None,
    }
}

fn authoritative_reasoning_capabilities(
    params: Option<serde_json::Value>,
    kind: BackendProviderKind,
    advertised: Option<&[ReasoningEffort]>,
) -> Option<serde_json::Value> {
    if !kind.uses_messages_wire() {
        return params;
    }
    let mut params = params.unwrap_or_else(|| serde_json::json!({}));
    params[crate::inference::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM] =
        advertised.map_or(serde_json::Value::Null, |values| {
            serde_json::Value::Array(
                values
                    .iter()
                    .map(|value| serde_json::Value::String(value.as_str().to_string()))
                    .collect(),
            )
        });
    Some(params)
}

pub(crate) fn loop_config_for_request(
    agent_config: &ResolvedAgent,
    preamble: String,
    request: &AgentRequest,
    aggregate_token_budget: Option<AggregateTokenBudget>,
    tool_count: usize,
) -> anyhow::Result<LoopConfig> {
    let mut config = loop_config(
        agent_config,
        preamble,
        tool_count,
        CaptureScopeKind::Inference,
    );
    agent_config.sampling.validate_for_provider(
        agent_config.backend_provider_kind,
        agent_config.openai_wire_api,
    )?;
    config.aggregate_token_budget = aggregate_token_budget;
    let request_additional_params = request_additional_params(agent_config, request);
    if let Some(additional_params) = request_additional_params {
        config.additional_params =
            merge_optional_params(config.additional_params.take(), Some(additional_params));
    }
    let origin = ExecutionOrigin::from_persisted(request.execution_origin.as_deref())?;
    config.retry_policy = CompletionRetryPolicy::resolve(&agent_config.completion_retry, origin);
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

/// Mint the single monotone provider-usage ledger for one durable request,
/// rehydrated from its signed physical scope and authenticated auxiliary
/// children so crash redrive cannot mint another allowance.
pub(crate) async fn aggregate_token_budget_for_request(
    node: &EmbeddedNode,
    request: &AgentRequest,
) -> anyhow::Result<Option<AggregateTokenBudget>> {
    use gents_protocol::request_admission::RequestPurpose;

    anyhow::ensure!(
        request.purpose != RequestPurpose::AccessReview,
        "access review must borrow its caller's token ledger"
    );
    let Some(limit) = parse_aggregate_token_limit(request.max_total_tokens)? else {
        return Ok(None);
    };
    let used = match request.purpose {
        RequestPurpose::Normal => {
            crate::toolset::load_parent_inclusive_budget_usage(node, request).await?
        }
        RequestPurpose::TitleAudit => load_prior_charged_tokens(node, &request.doc_id).await?,
        RequestPurpose::AccessReview => unreachable!("review cannot mint a ledger"),
    };
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
    let resp =
        crate::graphql::graphql_with_transaction_retry(node, &query, "load_prior_charged_tokens")
            .await?;
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

#[derive(Debug, Deserialize)]
struct CalibrationCallRow {
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
    #[serde(default)]
    cached_input_tokens: Option<i64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<i64>,
    #[serde(default)]
    usage_completeness: Option<String>,
    #[serde(default)]
    context_accounting_json: Option<String>,
}

/// Inference calls scanned for the session's calibration window; most carry
/// usable usage, so only failed or unaccounted calls are skipped.
const CALIBRATION_CALL_SCAN: usize =
    4 * crate::provider_input::budget::CALIBRATION_OBSERVATION_WINDOW;

/// The provider-input calibration a request starts from, from this session's
/// inference calls with this agent's model and estimator: the ratios of the
/// newest calls with usable usage fill the observation window, and the newest
/// informative call anchors when it has usable usage. Without any, the
/// newest call's dispatched ratio, else the profile's prior, and no anchor.
/// Calibration only raises estimates, so an unreadable history keeps the
/// prior rather than failing the request.
pub(crate) async fn context_calibration_for_session(
    node: &EmbeddedNode,
    agent_config: &ResolvedAgent,
    node_did: &str,
    session_id: &str,
) -> crate::provider_input::budget::ContextCalibration {
    let profile = crate::provider_input::ProviderInputProfile::resolve(
        agent_config.backend_provider_kind,
        agent_config.openai_wire_api,
    );
    match load_session_calibration_rows(
        node,
        node_did,
        &agent_config.agent_id,
        session_id,
        &agent_config.model_name,
    )
    .await
    {
        Ok(rows) => {
            let seed = session_calibration_seed(&rows, profile.estimator_name());
            let prior = seed.dispatched_ratio.unwrap_or_else(|| {
                crate::provider_input::budget::ContextCalibration::prior_for(profile)
                    .ratio_basis_points()
            });
            let calibration = crate::provider_input::budget::ContextCalibration::with_observations(
                prior,
                seed.observations,
            );
            match seed.anchor {
                Some(anchor) => calibration.anchored_at(anchor),
                None => calibration,
            }
        }
        Err(error) => {
            tracing::warn!(
                session_id,
                error = format!("{error:#}"),
                "loading the session's provider-input calibration failed; using the profile prior"
            );
            crate::provider_input::budget::ContextCalibration::prior_for(profile)
        }
    }
}

/// The session's inference calls made by this agent with this model, newest
/// first. A session can be run by more than one agent (handoffs, tests);
/// another agent's calls describe another preamble and tool set, so they
/// neither observe nor anchor this one.
async fn load_session_calibration_rows(
    node: &EmbeddedNode,
    node_did: &str,
    agent_id: &str,
    session_id: &str,
    model_name: &str,
) -> anyhow::Result<Vec<CalibrationCallRow>> {
    let query = format!(
        r#"{{
            InferenceCall(
                filter: {{ _and: [
                    {{ node_did: {{ _eq: "{}" }} }},
                    {{ agent_id: {{ _eq: "{}" }} }},
                    {{ session_id: {{ _eq: "{}" }} }},
                    {{ model_name: {{ _eq: "{}" }} }},
                    {{ call_kind: {{ _eq: "inference" }} }}
                ] }},
                order: [{{ queued_at: DESC }}, {{ call_seq: DESC }}],
                limit: {CALIBRATION_CALL_SCAN}
            ) {{
                prompt_tokens
                completion_tokens
                cached_input_tokens
                cache_creation_input_tokens
                usage_completeness
                context_accounting_json
            }}
        }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(agent_id),
        escape_graphql_string(session_id),
        escape_graphql_string(model_name),
    );
    let response = crate::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "load_session_calibration_rows",
    )
    .await?;
    match response
        .data
        .as_ref()
        .and_then(|data| data.get("InferenceCall"))
    {
        None | Some(serde_json::Value::Null) => Ok(Vec::new()),
        Some(rows) => serde_json::from_value(rows.clone()).map_err(|error| {
            anyhow::anyhow!("InferenceCall calibration payload is not a row array: {error}")
        }),
    }
}

/// What a session's recorded inference calls say about its calibration.
#[derive(Debug, Default, PartialEq, Eq)]
struct CalibrationSeed {
    /// Provider/estimate ratios of the newest calls with usable usage,
    /// oldest first, at most the calibration window.
    observations: Vec<u64>,
    /// The ratio the newest informative call was dispatched with: the prior
    /// when no call in the scan has usable usage.
    dispatched_ratio: Option<u64>,
    /// The newest informative call, when it has usable usage: an older call
    /// is not a prefix of what followed the newest one.
    anchor: Option<crate::provider_input::budget::ContextAnchor>,
}

/// One recorded call of this estimator: its usable usage as a ratio and
/// anchor, and the ratio it was dispatched with.
struct RecordedCalibration {
    observation: Option<(u64, crate::provider_input::budget::ContextAnchor)>,
    dispatched_ratio: Option<u64>,
}

/// A call counts as an observation when it recorded complete,
/// contract-shaped usage at or above its own raw estimate. Rows from another
/// estimator are a different scale. Rows written before Claude usage was
/// normalized hold uncached input below the cache counters, and an input
/// under the estimate is implausible for an undercounting estimator; neither
/// is an observation, so neither can lower the ratio.
fn recorded_calibration(row: &CalibrationCallRow, estimator: &str) -> Option<RecordedCalibration> {
    let accounting = serde_json::from_str::<gents_protocol::rendered_request::ContextAccounting>(
        row.context_accounting_json.as_deref()?,
    )
    .ok()
    .filter(|accounting| accounting.estimator == estimator)?;
    let estimated = accounting
        .raw_estimated_input_tokens
        .unwrap_or(accounting.estimated_input_tokens);
    let counter = |value: Option<i64>| value.and_then(|value| u64::try_from(value).ok());
    let observation = (row.usage_completeness.as_deref() == Some("complete"))
        .then_some(())
        .and_then(|()| {
            provider_usage::contract_input_tokens(
                counter(row.prompt_tokens)?,
                counter(row.cached_input_tokens).unwrap_or(0),
                counter(row.cache_creation_input_tokens).unwrap_or(0),
            )
        })
        .filter(|observed| *observed >= estimated as u64)
        .and_then(|observed| {
            let ratio = crate::provider_input::budget::observed_input_ratio_basis_points(
                observed, estimated,
            )?;
            Some((
                ratio,
                crate::provider_input::budget::ContextAnchor {
                    input_tokens: observed,
                    output_tokens: counter(row.completion_tokens).unwrap_or(0),
                    estimated_input_tokens: estimated,
                },
            ))
        });
    Some(RecordedCalibration {
        observation,
        dispatched_ratio: accounting.input_calibration_basis_points,
    })
}

/// The calibration seed of `rows`, newest first.
fn session_calibration_seed(rows: &[CalibrationCallRow], estimator: &str) -> CalibrationSeed {
    let recorded = rows
        .iter()
        .filter_map(|row| recorded_calibration(row, estimator))
        .filter(|recorded| recorded.observation.is_some() || recorded.dispatched_ratio.is_some())
        .collect::<Vec<_>>();
    let mut observations = recorded
        .iter()
        .filter_map(|recorded| recorded.observation.map(|(ratio, _)| ratio))
        .take(crate::provider_input::budget::CALIBRATION_OBSERVATION_WINDOW)
        .collect::<Vec<_>>();
    observations.reverse();
    CalibrationSeed {
        observations,
        dispatched_ratio: recorded.first().and_then(|newest| newest.dispatched_ratio),
        anchor: recorded
            .first()
            .and_then(|newest| newest.observation.map(|(_, anchor)| anchor)),
    }
}

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

pub(crate) use gents_loop::compaction::merge_optional_params;

/// Maps the inference profile's reasoning effort into each provider's wire
/// contract. An absent profile setting injects no reasoning default, except
/// ChatGPT Codex (#540).
///
/// vLLM's OpenAI-compatible server only emits chain-of-thought in
/// `message.reasoning` when the request carries
/// `chat_template_kwargs={"enable_thinking": true}`; that local-only toggle
/// must not be sent to Responses or OpenRouter endpoints, which use the
/// standard `reasoning.effort` object instead. The key is serialized flat into
/// the OpenAI completion body, so it reaches vLLM as a top-level
/// `chat_template_kwargs` object — exactly where the server reads it.
fn reasoning_profile_params(
    kind: BackendProviderKind,
    wire_api: OpenAiWireApi,
    reasoning_effort: Option<ReasoningEffort>,
) -> Option<serde_json::Value> {
    let Some(reasoning_effort) = reasoning_effort else {
        // Profile plumbing may override Codex's shipped `medium` default,
        // never silently drop it (#540).
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
        // Grok models 400 on a forced reasoning.effort.
        (BackendProviderKind::XaiGrokOAuth, _) => None,
        (BackendProviderKind::ClaudeCliSubscription | BackendProviderKind::AnthropicApiKey, _) => {
            Some(serde_json::json!({
            "output_config": { "effort": reasoning_effort.as_str() }
            }))
        }
    }
}

/// Whether final rendered provider params explicitly switch reasoning off.
/// Claude's `output_config.effort` does not switch thinking off, and Grok
/// receives no effort, so neither counts.
pub(crate) fn params_disable_reasoning(params: Option<&serde_json::Value>) -> bool {
    let Some(params) = params else {
        return false;
    };
    if let Some(enabled) = params
        .pointer("/chat_template_kwargs/enable_thinking")
        .and_then(serde_json::Value::as_bool)
    {
        return !enabled;
    }
    params
        .pointer("/reasoning/effort")
        .and_then(serde_json::Value::as_str)
        == Some(ReasoningEffort::None.as_str())
}

/// xAI retains Responses requests by default, so an API-key backend at its
/// endpoint asks for `store: false` plus encrypted reasoning to keep reasoning
/// replayable without server state (`PromptAssembly.ResponsesStorage.storage`).
fn provider_additional_params(
    kind: BackendProviderKind,
    wire: crate::OpenAiWireApi,
    endpoint: &str,
) -> Option<serde_json::Value> {
    match kind {
        BackendProviderKind::OpenAiCompatible
            if wire == crate::OpenAiWireApi::Responses
                && crate::inference::backends::setup::is_xai_api_endpoint(endpoint) =>
        {
            Some(serde_json::json!({
                "store": false,
                "include": [gents_loop::provider_patches::ENCRYPTED_REASONING_INCLUDE],
            }))
        }
        BackendProviderKind::OpenAiCompatible => None,
        BackendProviderKind::OpenRouter => Some(serde_json::json!({
            "provider": { "require_parameters": true }
        })),
        BackendProviderKind::ChatGptCodex
        | BackendProviderKind::XaiGrokOAuth
        | BackendProviderKind::ClaudeCliSubscription
        | BackendProviderKind::AnthropicApiKey => None,
    }
}

fn request_additional_params(
    agent_config: &ResolvedAgent,
    request: &AgentRequest,
) -> Option<serde_json::Value> {
    match agent_config.backend_provider_kind {
        BackendProviderKind::OpenAiCompatible => openai_cache_scope_params(request),
        BackendProviderKind::ChatGptCodex => {
            let scope = normalize_cache_scope(request.session_id.as_str())
                .or_else(|| normalize_cache_scope(request.request_id.as_str()))?;
            Some(serde_json::json!({ "prompt_cache_key": scope }))
        }
        BackendProviderKind::OpenRouter
        | BackendProviderKind::XaiGrokOAuth
        | BackendProviderKind::ClaudeCliSubscription
        | BackendProviderKind::AnthropicApiKey => None,
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

/// The enclosing request later supplies deadline and ledger.
pub(crate) async fn build_compaction_engine(
    cache: std::sync::Arc<crate::inference::usage_observation::UsageCache>,
    node: std::sync::Arc<EmbeddedNode>,
    agent_config: &ResolvedAgent,
    admission: AdmissionRegistry,
    build_timeout: std::time::Duration,
) -> anyhow::Result<
    Option<(
        std::sync::Arc<dyn crate::inference::compaction::ReductionEngine>,
        String,
    )>,
> {
    let Some(inference) = &agent_config.compaction_inference else {
        anyhow::ensure!(
            matches!(
                agent_config.compaction_strategy().reduction_mode(),
                crate::inference::compaction::ReductionMode::StripOnly
            ) || agent_config
                .compaction
                .as_ref()
                .and_then(|config| config.inference_profile_id.as_ref())
                .is_none(),
            "selected compaction inference profile was not resolved"
        );
        return Ok(None);
    };
    if matches!(
        agent_config.compaction_strategy().reduction_mode(),
        crate::inference::compaction::ReductionMode::StripOnly
    ) {
        return Ok(None);
    }
    anyhow::ensure!(
        inference.backend.node_did == agent_config.node_did()
            && inference.profile.node_did == agent_config.node_did(),
        "summary inference must belong to the invoking node"
    );
    anyhow::ensure!(
        inference.profile.backend_id == inference.backend.backend_id,
        "summary profile/backend reference mismatch"
    );
    inference.backend.validate()?;
    let mut summary = agent_config.clone();
    let backend = inference.backend.backend_fields();
    summary.backend_id = backend.backend_id;
    summary.backend_provider_kind = backend.backend_provider_kind;
    summary.openai_wire_api = backend.openai_wire_api;
    summary.backend_endpoint = backend.backend_endpoint;
    summary.backend_auth = backend.backend_auth;
    summary.model_name = inference.profile.model_name.clone();
    summary.profile_id = Some(inference.profile.profile_id.clone());
    summary.service_tier = inference.profile.service_tier;
    summary.resolved_reasoning_efforts = inference.resolved_reasoning_efforts();
    summary.context_window = inference.context_window()?;
    summary.max_output_tokens = inference.max_output_tokens()?;
    summary.sampling = inference.sampling_config()?;
    summary.max_turns = 0;
    summary.compaction_inference = None;
    let api_key = match &summary.backend_auth {
        crate::documents::BackendAuth::NodeOAuth { .. } => "no-key".to_owned(),
        _ => summary.completion_client_api_key()?,
    };
    let client = crate::inference::llm::backend_client::build_backend_client(
        cache,
        node,
        &summary,
        &api_key,
        build_timeout,
    )
    .await?;
    let provider_family = client.provider_family().to_owned();
    let source_counter = std::sync::Arc::new(crate::provider_input::ProviderInputCounter::new(
        agent_config.backend_provider_kind,
        agent_config.openai_wire_api,
        agent_config.model_name.clone(),
    ));
    let config = loop_config(&summary, String::new(), 0, CaptureScopeKind::Compaction);
    let backend_id = inference.backend.backend_id.clone();
    let engine = crate::inference::llm::backend_client::with_backend_client!(client, |client| {
        let model = std::sync::Arc::new(build_admitted_model(client, admission, &summary));
        crate::inference::compaction::backend_scoped_reduction_engine(
            std::sync::Arc::new(
                crate::inference::compaction::ProviderReductionEngine::new(model, config)
                    .with_source_input_counter(source_counter)
                    .with_summary_output_limit(inference.max_output_tokens()?),
            ),
            backend_id,
        ) as std::sync::Arc<dyn crate::inference::compaction::ReductionEngine>
    });
    Ok(Some((engine, provider_family)))
}
