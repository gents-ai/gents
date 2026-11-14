use gents_store::config_client::GraphqlEndpoint;
use std::collections::BTreeSet;

use anyhow::Result;
use gents_protocol::row::{AgentRequestRow, CompactionEntryRow};
use serde::{Deserialize, Serialize};

use gents_store::config_client::ConfigAccess;

/// Bound the request scan used to discover sessions for this context estimate;
/// the response reports this limit so callers can interpret its coverage.
const RECENT_REQUEST_SCAN: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelfAgent {
    pub agent_did: String,
    pub display_name: String,
    pub model_name: String,
    pub enabled: bool,
    pub backend_id: String,
    pub provider_kind: String,
    pub endpoint: String,
    pub inference_profile_id: String,
    pub context_window: Option<i64>,
    pub compaction_threshold: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBudget {
    pub compaction_count: i64,
    pub latest_compaction_at: Option<String>,
    pub latest_original_tokens: Option<i64>,
    pub latest_compacted_tokens: Option<i64>,
    pub sessions_considered: i64,
    pub request_scan_limit: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ContextIndicator {
    pub max_tokens: Option<i64>,
    pub current_estimate: Option<i64>,
    pub utilization_percent: Option<f64>,
    pub compaction_count: i64,
    pub last_compacted_at: Option<String>,
}

pub async fn load_self_view(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<(Vec<SelfAgent>, ContextBudget, ContextIndicator)> {
    let access = ConfigAccess::Graphql(graphql.clone());
    access
        .transact_readonly("views.http.self", |txn| {
            Box::pin(async move {
                let (agents, backends, profiles, contexts, configs, requests) = txn
                    .read_views(&(
                        gents_documents::views::agents::AgentList::new(node_did),
                        gents_inference::views::backends::BackendList::new(node_did),
                        gents_inference::views::profiles::ProfileList::new(node_did),
                        gents_documents::views::config::AgentContextList::new(node_did),
                        gents_documents::views::config::CompactionConfigList::new(node_did),
                        gents_session::views::requests::RequestRows::new(
                            gents_session::views::transcript::Scope::Node(node_did.to_owned()),
                        )
                        .limit(RECENT_REQUEST_SCAN as u64),
                    ))
                    .await?;
                let agents = build_agents(
                    agents.into_iter().map(|row| row.agent).collect(),
                    backends.into_iter().map(|row| row.metadata).collect(),
                    profiles.into_iter().map(|row| row.profile).collect(),
                    contexts.into_iter().map(|row| row.document).collect(),
                    configs.into_iter().map(|row| row.document).collect(),
                );
                let session_ids = distinct_session_ids(&requests);
                let mut budget = if session_ids.is_empty() {
                    ContextBudget::default()
                } else {
                    let mut view = gents_session::views::transcript::CompactionRows::new(
                        gents_session::views::transcript::Scope::Node(node_did.to_owned()),
                    );
                    view.session_ids = Some(session_ids.clone());
                    aggregate_compaction(txn.read_view(&view).await?)
                };
                budget.sessions_considered = session_ids.len() as i64;
                budget.request_scan_limit = RECENT_REQUEST_SCAN as i64;
                let context = build_context_indicator(&agents, &budget);
                Ok::<_, anyhow::Error>((agents, budget, context))
            })
        })
        .await
}

fn build_agents(
    agents: Vec<gents_documents::Agent>,
    backends: Vec<gents_documents::InferenceBackendMetadata>,
    profiles: Vec<gents_documents::InferenceProfile>,
    contexts: Vec<gents_documents::AgentContext>,
    compaction_configs: Vec<gents_documents::CompactionConfig>,
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
            let agent_did = agent_config.agent_did.trim().to_string();
            if agent_did.is_empty() {
                return None;
            }
            let inference_profile_id = agent_config.inference_profile_id.trim().to_string();
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
                .map(|profile| profile.backend_id.as_str())
                .unwrap_or_default()
                .trim()
                .to_string();
            let backend = backends.get(&backend_id);
            Some(SelfAgent {
                agent_did,
                display_name: agent_config.display_name.unwrap_or_default(),
                model_name: profile
                    .map(|profile| profile.model_name.clone())
                    .unwrap_or_default(),
                enabled: agent_config.enabled,
                backend_id,
                provider_kind: backend
                    .map(|backend| backend.provider_kind.as_str().to_owned())
                    .unwrap_or_default(),
                endpoint: backend
                    .map(|backend| backend.endpoint.clone())
                    .unwrap_or_default(),
                inference_profile_id,
                context_window: profile.and_then(|profile| profile.context_window),
                compaction_threshold: compaction
                    .and_then(|config| config.threshold)
                    .unwrap_or(gents_inference::resolved::DEFAULT_COMPACTION_THRESHOLD),
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

fn aggregate_compaction(compactions: Vec<CompactionEntryRow>) -> ContextBudget {
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
