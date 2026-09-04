use std::collections::BTreeSet;

use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

const RECENT_REQUEST_SCAN: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct SelfBehavior {
    pub(crate) behavior_id: String,
    pub(crate) display_name: String,
    pub(crate) model_name: String,
    pub(crate) enabled: bool,
    pub(crate) backend_id: String,
    pub(crate) provider_kind: String,
    pub(crate) endpoint: String,
    pub(crate) inference_profile_id: String,
    pub(crate) context_window: Option<i64>,
    /// Compaction threshold fraction (0.0-1.0) sourced from the behavior row,
    /// falling back to `gents::config::DEFAULT_COMPACTION_THRESHOLD` when the
    /// row leaves it unset.
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
    /// The effective input budget (context window scaled by the compaction
    /// threshold) of the primary behavior, not the raw context window.
    pub(crate) max_tokens: Option<i64>,
    pub(crate) current_estimate: Option<i64>,
    pub(crate) utilization_percent: Option<f64>,
    pub(crate) compaction_count: i64,
    pub(crate) last_compacted_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SelfViewEnvelope {
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorRow>,
    #[serde(rename = "InferenceBackend", default)]
    backends: Vec<BackendRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<AgentRequestRow>,
}

#[derive(Debug, Deserialize)]
struct CompactionEnvelope {
    #[serde(rename = "CompactionEntry", default)]
    compactions: Vec<CompactionRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct BehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    backend_id: Option<String>,
    #[serde(default)]
    inference_profile_id: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    compaction_threshold: Option<f64>,
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
    context_window: Option<i64>,
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
    graphql: &str,
    agent_did: &str,
) -> Result<(Vec<SelfBehavior>, ContextBudget, ContextIndicator)> {
    let response = post_graphql(graphql, &self_view_query(agent_did)).await?;
    let envelope = decode::<SelfViewEnvelope>(response, "self view")?;

    let behaviors = build_behaviors(envelope.behaviors, envelope.backends, envelope.profiles);
    let session_ids = distinct_session_ids(&envelope.requests);

    let mut context_budget = if session_ids.is_empty() {
        ContextBudget::default()
    } else {
        let response = post_graphql(graphql, &compaction_query(&session_ids)).await?;
        let envelope = decode::<CompactionEnvelope>(response, "context budget")?;
        aggregate_compaction(envelope.compactions)
    };
    context_budget.sessions_considered = session_ids.len() as i64;
    context_budget.request_scan_limit = RECENT_REQUEST_SCAN as i64;
    let context = build_context_indicator(&behaviors, &context_budget);

    Ok((behaviors, context_budget, context))
}

fn self_view_query(agent_did: &str) -> String {
    let agent_did = escape_graphql_string(agent_did);
    format!(
        r#"{{
        AgentBehavior(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, order: {{ behavior_id: ASC }}) {{
            behavior_id
            display_name
            model_name
            backend_id
            inference_profile_id
            enabled
            compaction_threshold
        }}
        InferenceBackend(order: {{ backend_id: ASC }}) {{
            backend_id
            provider_kind
            endpoint
        }}
        InferenceProfile(order: {{ profile_id: ASC }}) {{
            profile_id
            context_window
        }}
        AgentRequest(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}, order: {{ created_at: DESC }}, limit: {RECENT_REQUEST_SCAN}) {{
            request_id
            session_id
        }}
    }}"#
    )
}

fn compaction_query(session_ids: &[String]) -> String {
    let list = session_ids
        .iter()
        .map(|id| format!(r#""{}""#, escape_graphql_string(id)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"{{
        CompactionEntry(filter: {{ session_id: {{ _in: [{list}] }} }}, order: {{ created_at: DESC }}) {{
            created_at
            original_tokens
            compacted_tokens
        }}
    }}"#
    )
}

fn decode<T: serde::de::DeserializeOwned>(response: Value, label: &str) -> Result<T> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("{label} query response missing object data: {response}"))?;
    serde_json::from_value(data).with_context(|| format!("decoding {label} query response"))
}

fn build_behaviors(
    behaviors: Vec<BehaviorRow>,
    backends: Vec<BackendRow>,
    profiles: Vec<ProfileRow>,
) -> Vec<SelfBehavior> {
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

    behaviors
        .into_iter()
        .filter_map(|behavior| {
            let behavior_id = behavior.behavior_id.trim().to_string();
            if behavior_id.is_empty() {
                return None;
            }
            let backend_id = behavior.backend_id.unwrap_or_default().trim().to_string();
            let inference_profile_id = behavior
                .inference_profile_id
                .unwrap_or_default()
                .trim()
                .to_string();
            let backend = backends.get(&backend_id);
            let profile = profiles.get(&inference_profile_id);
            Some(SelfBehavior {
                behavior_id,
                display_name: behavior.display_name.unwrap_or_default(),
                model_name: behavior.model_name.unwrap_or_default(),
                enabled: behavior.enabled.unwrap_or(true),
                backend_id,
                provider_kind: backend
                    .and_then(|backend| backend.provider_kind.clone())
                    .unwrap_or_default(),
                endpoint: backend
                    .and_then(|backend| backend.endpoint.clone())
                    .unwrap_or_default(),
                inference_profile_id,
                context_window: profile.and_then(|profile| profile.context_window),
                compaction_threshold: behavior
                    .compaction_threshold
                    .unwrap_or(gents::config::DEFAULT_COMPACTION_THRESHOLD),
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
    behaviors: &[SelfBehavior],
    context_budget: &ContextBudget,
) -> ContextIndicator {
    // max_tokens is the effective input budget (context window scaled by the
    // compaction threshold) of the enabled behavior with the largest window,
    // not the raw context window: utilization is measured against the same
    // budget the runtime dispatches against.
    let max_tokens = behaviors
        .iter()
        .filter(|behavior| behavior.enabled)
        .filter(|behavior| behavior.context_window.is_some_and(|value| value > 0))
        .max_by_key(|behavior| behavior.context_window.unwrap_or_default())
        .map(|behavior| {
            let context_window = behavior.context_window.unwrap_or_default().max(0) as usize;
            let budget = gents::provider_budget::effective_input_budget(
                context_window,
                behavior.compaction_threshold,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rows<T: serde::de::DeserializeOwned>(value: Value) -> Vec<T> {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn joins_behavior_with_backend_and_profile() {
        let behaviors = build_behaviors(
            rows(json!([{
                "behavior_id": "amy-general",
                "display_name": "Amy General",
                "model_name": "gpt-4",
                "backend_id": "b1",
                "inference_profile_id": "p1",
                "enabled": true
            }])),
            rows(json!([{
                "backend_id": "b1",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://host/v1"
            }])),
            rows(json!([{ "profile_id": "p1", "context_window": 128000 }])),
        );

        assert_eq!(behaviors.len(), 1);
        let b = &behaviors[0];
        assert_eq!(b.model_name, "gpt-4");
        assert_eq!(b.provider_kind, "OpenAiCompatible");
        assert_eq!(b.endpoint, "http://host/v1");
        assert_eq!(b.context_window, Some(128000));
        assert_eq!(
            b.compaction_threshold,
            gents::config::DEFAULT_COMPACTION_THRESHOLD
        );
    }

    #[test]
    fn behavior_without_matching_backend_or_profile_has_empty_join() {
        let behaviors = build_behaviors(
            rows(json!([{ "behavior_id": "orphan", "backend_id": "missing" }])),
            rows(json!([])),
            rows(json!([])),
        );
        assert_eq!(behaviors.len(), 1);
        assert_eq!(behaviors[0].provider_kind, "");
        assert_eq!(behaviors[0].endpoint, "");
        assert_eq!(behaviors[0].context_window, None);
        assert!(behaviors[0].enabled);
        assert_eq!(
            behaviors[0].compaction_threshold,
            gents::config::DEFAULT_COMPACTION_THRESHOLD
        );
    }

    #[test]
    fn distinct_session_ids_dedupes_and_drops_empty() {
        let ids = distinct_session_ids(&rows(json!([
            { "request_id": "r-1", "session_id": "s-a" },
            { "request_id": "r-2", "session_id": "s-a" },
            { "request_id": "r-3", "session_id": "s-b" },
            { "request_id": "r-4", "session_id": "" },
            { "request_id": "r-5", "session_id": null }
        ])));
        assert_eq!(ids, vec!["s-a".to_string(), "s-b".to_string()]);
    }

    #[test]
    fn self_view_rejects_request_without_request_id() {
        let error = decode::<SelfViewEnvelope>(
            json!({ "data": { "AgentRequest": [{ "session_id": "s-a" }] } }),
            "self view",
        )
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("missing field `request_id`"),
            "{error:#}"
        );
    }

    #[test]
    fn context_budget_counts_and_picks_latest_compaction() {
        let budget = aggregate_compaction(rows(json!([
            { "created_at": "2026-06-01T10:00:00Z", "original_tokens": 100, "compacted_tokens": 40 },
            { "created_at": "2026-06-02T10:00:00Z", "original_tokens": 200, "compacted_tokens": 80 }
        ])));
        assert_eq!(budget.compaction_count, 2);
        assert_eq!(
            budget.latest_compaction_at.as_deref(),
            Some("2026-06-02T10:00:00Z")
        );
        assert_eq!(budget.latest_original_tokens, Some(200));
        assert_eq!(budget.latest_compacted_tokens, Some(80));
    }

    #[test]
    fn empty_compactions_yield_default_budget() {
        let budget = aggregate_compaction(rows(json!([])));
        assert_eq!(budget.compaction_count, 0);
        assert_eq!(budget.latest_compaction_at, None);
    }

    #[test]
    fn context_indicator_uses_enabled_behavior_window_and_latest_compaction() {
        let behaviors = vec![
            SelfBehavior {
                behavior_id: "disabled".to_string(),
                display_name: String::new(),
                model_name: String::new(),
                enabled: false,
                backend_id: String::new(),
                provider_kind: String::new(),
                endpoint: String::new(),
                inference_profile_id: String::new(),
                context_window: Some(2000),
                compaction_threshold: 0.75,
            },
            SelfBehavior {
                behavior_id: "enabled".to_string(),
                display_name: String::new(),
                model_name: String::new(),
                enabled: true,
                backend_id: String::new(),
                provider_kind: String::new(),
                endpoint: String::new(),
                inference_profile_id: String::new(),
                context_window: Some(1000),
                compaction_threshold: 0.8,
            },
        ];
        let budget = ContextBudget {
            compaction_count: 2,
            latest_compaction_at: Some("2026-06-03T10:30:00Z".to_string()),
            latest_original_tokens: Some(800),
            latest_compacted_tokens: Some(400),
            sessions_considered: 1,
            request_scan_limit: RECENT_REQUEST_SCAN as i64,
        };

        let context = build_context_indicator(&behaviors, &budget);

        // max_tokens is the effective input budget of the enabled behavior
        // with the largest window (1000 * 0.8 == 800), not the raw window.
        assert_eq!(context.max_tokens, Some(800));
        assert_eq!(context.current_estimate, Some(400));
        assert_eq!(context.utilization_percent, Some(50.0));
        assert_eq!(context.compaction_count, 2);
        assert_eq!(
            context.last_compacted_at.as_deref(),
            Some("2026-06-03T10:30:00Z")
        );
    }

    #[test]
    fn context_indicator_utilization_is_measured_against_the_effective_input_budget() {
        let behaviors = vec![SelfBehavior {
            behavior_id: "enabled".to_string(),
            display_name: String::new(),
            model_name: String::new(),
            enabled: true,
            backend_id: String::new(),
            provider_kind: String::new(),
            endpoint: String::new(),
            inference_profile_id: String::new(),
            context_window: Some(100_000),
            compaction_threshold: 0.75,
        }];
        let budget = ContextBudget {
            compaction_count: 0,
            latest_compaction_at: None,
            latest_original_tokens: Some(60_000),
            latest_compacted_tokens: None,
            sessions_considered: 1,
            request_scan_limit: RECENT_REQUEST_SCAN as i64,
        };

        let context = build_context_indicator(&behaviors, &budget);

        // Budget is 100_000 * 0.75 == 75_000, so 60_000 current tokens is
        // 80% utilized, not 60% of the raw 100_000 window.
        assert_eq!(context.max_tokens, Some(75_000));
        assert_eq!(context.current_estimate, Some(60_000));
        assert_eq!(context.utilization_percent, Some(80.0));
    }
}
