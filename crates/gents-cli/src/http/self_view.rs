use gents::config_client::GraphqlEndpoint;
use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

/// Bound the request scan used to discover sessions for this context estimate;
/// the response reports this limit so callers can interpret its coverage.
const RECENT_REQUEST_SCAN: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SelfAgent {
    pub(crate) agent_id: String,
    pub(crate) display_name: String,
    pub(crate) model_name: String,
    pub(crate) enabled: bool,
    pub(crate) backend_id: String,
    pub(crate) provider_kind: String,
    pub(crate) endpoint: String,
    pub(crate) inference_profile_id: String,
    pub(crate) context_window: Option<i64>,
    pub(crate) compaction_threshold: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ContextBudget {
    pub(crate) compaction_count: i64,
    pub(crate) latest_compaction_at: Option<String>,
    pub(crate) latest_original_tokens: Option<i64>,
    pub(crate) latest_compacted_tokens: Option<i64>,
    pub(crate) sessions_considered: i64,
    pub(crate) request_scan_limit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub(crate) struct ContextIndicator {
    pub(crate) max_tokens: Option<i64>,
    pub(crate) current_estimate: Option<i64>,
    pub(crate) utilization_percent: Option<f64>,
    pub(crate) compaction_count: i64,
    pub(crate) last_compacted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SelfViewEnvelope {
    #[serde(rename = "Agent", default)]
    agents: Vec<AgentRow>,
    #[serde(rename = "InferenceBackend", default)]
    backends: Vec<BackendRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileRow>,
    #[serde(rename = "AgentContext", default)]
    contexts: Vec<ContextRow>,
    #[serde(rename = "CompactionConfig", default)]
    compaction_configs: Vec<CompactionConfigRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
}

#[derive(Debug, Deserialize)]
struct CompactionEnvelope {
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct AgentRow {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    context_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
struct BackendRow {
    #[serde(default)]
    backend_id: String,
    #[serde(default)]
    provider_kind: Option<String>,
    #[serde(default)]
    endpoint: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ProfileRow {
    #[serde(default)]
    profile_id: String,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    context_window: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
struct ContextRow {
    #[serde(default)]
    context_id: String,
    #[serde(default)]
    compaction_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionConfigRow {
    #[serde(default)]
    compaction_id: String,
    #[serde(default)]
    threshold: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
struct CompactionRow {
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    original_tokens: Option<i64>,
    #[serde(default)]
    compacted_tokens: Option<i64>,
}

pub(crate) async fn load_self_view(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<(Vec<SelfAgent>, ContextBudget, ContextIndicator)> {
    let response = post_graphql(graphql, &self_view_query(node_did)).await?;
    let envelope = decode::<SelfViewEnvelope>(response, "self view")?;

    let agents = build_agents(
        envelope.agents,
        envelope.backends,
        envelope.profiles,
        envelope.contexts,
        envelope.compaction_configs,
    );
    let session_ids = distinct_session_ids(&envelope.requests);

    let mut context_budget = if session_ids.is_empty() {
        ContextBudget::default()
    } else {
        let response = post_graphql(graphql, &compaction_query(node_did, &session_ids)).await?;
        let envelope = decode::<CompactionEnvelope>(response, "context budget")?;
        aggregate_compaction(envelope.compactions)
    };
    context_budget.sessions_considered = session_ids.len() as i64;
    context_budget.request_scan_limit = RECENT_REQUEST_SCAN as i64;
    let context = build_context_indicator(&agents, &context_budget);

    Ok((agents, context_budget, context))
}

fn self_view_query(node_did: &str) -> String {
    let node_did = escape_graphql_string(node_did);
    format!(
        r#"{{
        Agent(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, order: {{ agent_id: ASC }}) {{
            agent_id
            display_name
            inference_profile_id
            context_id
            enabled
        }}
        InferenceBackend(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, order: {{ backend_id: ASC }}) {{
            backend_id
            provider_kind
            endpoint
        }}
        InferenceProfile(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, order: {{ profile_id: ASC }}) {{
            profile_id
            backend_id
            model_name
            context_window
        }}
        AgentContext(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
            context_id
            compaction_id
        }}
        CompactionConfig(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{
            compaction_id
            threshold
        }}
        AgentRequest(filter: {{ node_did: {{ _eq: "{node_did}" }} }}, order: {{ created_at: DESC }}, limit: {RECENT_REQUEST_SCAN}) {{
            request_id
            session_id
        }}
    }}"#
    )
}

fn compaction_query(node_did: &str, session_ids: &[String]) -> String {
    let node_did = escape_graphql_string(node_did);
    let list = session_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
        CompactionEntry(filter: {{ _and: [
            {{ node_did: {{ _eq: "{node_did}" }} }},
            {{ session_id: {{ _in: [{list}] }} }}
        ] }}, order: {{ created_at: DESC }}) {{
            created_at
            original_tokens
            compacted_tokens
        }}
    }}"#
    )
}

fn decode<T>(response: Value, label: &str) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data: {response}"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn build_agents(
    agents: Vec<AgentRow>,
    backends: Vec<BackendRow>,
    profiles: Vec<ProfileRow>,
    contexts: Vec<ContextRow>,
    compaction_configs: Vec<CompactionConfigRow>,
) -> Vec<SelfAgent> {
    use std::collections::BTreeMap;

    let backends = backends
        .into_iter()
        .filter_map(|backend| {
            let backend_id = backend.backend_id.trim().to_string();
            (!backend_id.is_empty()).then_some((backend_id, backend))
        })
        .collect::<BTreeMap<_, _>>();
    let profiles = profiles
        .into_iter()
        .filter_map(|profile| {
            let profile_id = profile.profile_id.trim().to_string();
            (!profile_id.is_empty()).then_some((profile_id, profile))
        })
        .collect::<BTreeMap<_, _>>();
    let contexts = contexts
        .into_iter()
        .filter_map(|context| {
            let context_id = context.context_id.trim().to_string();
            (!context_id.is_empty()).then_some((context_id, context))
        })
        .collect::<BTreeMap<_, _>>();
    let compaction_configs = compaction_configs
        .into_iter()
        .filter_map(|config| {
            let compaction_id = config.compaction_id.trim().to_string();
            (!compaction_id.is_empty()).then_some((compaction_id, config))
        })
        .collect::<BTreeMap<_, _>>();

    agents
        .into_iter()
        .filter_map(|agent_config| {
            let agent_id = agent_config.agent_id.trim().to_string();
            if agent_id.is_empty() {
                return None;
            }
            let inference_profile_id = agent_config
                .inference_profile_id
                .unwrap_or_default()
                .trim()
                .to_string();
            let context_id = agent_config
                .context_id
                .as_deref()
                .unwrap_or_default()
                .trim();
            let context = contexts.get(context_id);
            let compaction = context
                .and_then(|context| context.compaction_id.as_deref())
                .and_then(|compaction_id| compaction_configs.get(compaction_id.trim()));
            let profile = profiles.get(&inference_profile_id);
            let backend_id = profile
                .and_then(|profile| profile.backend_id.as_deref())
                .unwrap_or_default()
                .trim()
                .to_string();
            let backend = backends.get(&backend_id);
            Some(SelfAgent {
                agent_id,
                display_name: agent_config.display_name.unwrap_or_default(),
                model_name: profile
                    .and_then(|profile| profile.model_name.clone())
                    .unwrap_or_default(),
                enabled: agent_config.enabled.unwrap_or(true),
                backend_id,
                provider_kind: backend
                    .and_then(|backend| backend.provider_kind.clone())
                    .unwrap_or_default(),
                endpoint: backend
                    .and_then(|backend| backend.endpoint.clone())
                    .unwrap_or_default(),
                inference_profile_id,
                context_window: profile.and_then(|profile| profile.context_window),
                compaction_threshold: compaction
                    .and_then(|config| config.threshold)
                    .unwrap_or(gents::inference::resolved::DEFAULT_COMPACTION_THRESHOLD),
            })
        })
        .collect()
}

fn distinct_session_ids(requests: &[AgentRequestRow]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| {
            let session_id = request.session_id.as_deref().unwrap_or_default().trim();
            (!session_id.is_empty()).then(|| session_id.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn aggregate_compaction(compactions: Vec<CompactionRow>) -> ContextBudget {
    let compaction_count = compactions.len() as i64;
    let latest = compactions
        .iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    ContextBudget {
        compaction_count,
        latest_compaction_at: latest.and_then(|entry| entry.created_at.clone()),
        latest_original_tokens: latest.and_then(|entry| entry.original_tokens),
        latest_compacted_tokens: latest.and_then(|entry| entry.compacted_tokens),
        ..Default::default()
    }
}

fn build_context_indicator(
    agents: &[SelfAgent],
    context_budget: &ContextBudget,
) -> ContextIndicator {
    let max_tokens = agents
        .iter()
        .filter(|agent_config| agent_config.enabled)
        .filter(|agent_config| agent_config.context_window.is_some_and(|value| value > 0))
        .max_by_key(|agent_config| agent_config.context_window.unwrap_or_default())
        .map(|agent_config| {
            let context_window = agent_config.context_window.unwrap_or_default().max(0) as usize;
            let budget = gents::provider_budget::effective_input_budget(
                context_window,
                agent_config.compaction_threshold,
            );
            i64::try_from(budget).unwrap_or(i64::MAX)
        });
    let current_estimate = context_budget
        .latest_compacted_tokens
        .or(context_budget.latest_original_tokens);
    let utilization_percent = match (current_estimate, max_tokens) {
        (Some(current), Some(max)) if max > 0 => Some((current as f64 / max as f64) * 100.0),
        _ => None,
    };

    ContextIndicator {
        max_tokens,
        current_estimate,
        utilization_percent,
        compaction_count: context_budget.compaction_count,
        last_compacted_at: context_budget.latest_compaction_at.clone(),
    }
}
