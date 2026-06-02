//! `/self` surfacing: joins the running agent's `AgentBehavior` rows with their
//! `InferenceBackend` (provider/endpoint) and `InferenceProfile`
//! (`context_window`), plus a context-budget summary derived from persisted
//! `CompactionEntry` rows. All of this data already exists; this is pure
//! surfacing onto the `/status` payload (and the `/self` alias).
//!
//! Note: `CompactionEntry` is keyed by `session_id` (not `agent_did`), so the
//! compaction summary is node-wide, not agent-scoped. Agent-scoping it would
//! require resolving the agent's sessions first — a follow-up if needed.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::post_graphql;

/// One of the running agent's behaviors with its backend + profile joined in.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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
}

/// Node-wide context-budget summary derived from `CompactionEntry`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ContextBudget {
    pub(crate) compaction_count: i64,
    pub(crate) latest_compaction_at: Option<String>,
    pub(crate) latest_original_tokens: Option<i64>,
    pub(crate) latest_compacted_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct SelfViewEnvelope {
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<BehaviorRow>,
    #[serde(rename = "InferenceBackend", default)]
    backends: Vec<BackendRow>,
    #[serde(rename = "InferenceProfile", default)]
    profiles: Vec<ProfileRow>,
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
) -> Result<(Vec<SelfBehavior>, ContextBudget)> {
    let response = post_graphql(graphql, &self_view_query(agent_did)).await?;
    let envelope = decode_self_view_response(response)?;
    Ok(build_self_view(envelope))
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
        CompactionEntry(order: {{ created_at: DESC }}) {{
            created_at
            original_tokens
            compacted_tokens
        }}
    }}"#
    )
}

fn decode_self_view_response(response: Value) -> Result<SelfViewEnvelope> {
    let data = response
        .get("data")
        .filter(|data| data.is_object())
        .cloned()
        .with_context(|| format!("self view query response missing object data: {response}"))?;
    serde_json::from_value(data).context("decoding self view query response")
}

fn build_self_view(envelope: SelfViewEnvelope) -> (Vec<SelfBehavior>, ContextBudget) {
    let backends = envelope
        .backends
        .into_iter()
        .filter_map(|backend| {
            let backend_id = backend.backend_id.trim().to_string();
            (!backend_id.is_empty()).then_some((backend_id, backend))
        })
        .collect::<BTreeMap<_, _>>();
    let profiles = envelope
        .profiles
        .into_iter()
        .filter_map(|profile| {
            let profile_id = profile.profile_id.trim().to_string();
            (!profile_id.is_empty()).then_some((profile_id, profile))
        })
        .collect::<BTreeMap<_, _>>();

    let behaviors = envelope
        .behaviors
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
                // Older behavior rows are enabled unless explicitly disabled.
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
            })
        })
        .collect();

    let compaction_count = envelope.compactions.len() as i64;
    // Latest by created_at (RFC3339 sorts lexically); independent of input order.
    let latest = envelope
        .compactions
        .iter()
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    let context_budget = ContextBudget {
        compaction_count,
        latest_compaction_at: latest.and_then(|entry| entry.created_at.clone()),
        latest_original_tokens: latest.and_then(|entry| entry.original_tokens),
        latest_compacted_tokens: latest.and_then(|entry| entry.compacted_tokens),
    };

    (behaviors, context_budget)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(value: Value) -> SelfViewEnvelope {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn joins_behavior_with_backend_and_profile() {
        let (behaviors, _budget) = build_self_view(envelope(json!({
            "AgentBehavior": [{
                "behavior_id": "amy-general",
                "display_name": "Amy General",
                "model_name": "gpt-4",
                "backend_id": "b1",
                "inference_profile_id": "p1",
                "enabled": true
            }],
            "InferenceBackend": [{
                "backend_id": "b1",
                "provider_kind": "OpenAiCompatible",
                "endpoint": "http://host/v1"
            }],
            "InferenceProfile": [{ "profile_id": "p1", "context_window": 128000 }],
            "CompactionEntry": []
        })));

        assert_eq!(behaviors.len(), 1);
        let b = &behaviors[0];
        assert_eq!(b.behavior_id, "amy-general");
        assert_eq!(b.model_name, "gpt-4");
        assert_eq!(b.provider_kind, "OpenAiCompatible");
        assert_eq!(b.endpoint, "http://host/v1");
        assert_eq!(b.context_window, Some(128000));
    }

    #[test]
    fn behavior_without_matching_backend_or_profile_has_empty_join() {
        let (behaviors, _) = build_self_view(envelope(json!({
            "AgentBehavior": [{ "behavior_id": "orphan", "backend_id": "missing" }],
            "InferenceBackend": [],
            "InferenceProfile": [],
            "CompactionEntry": []
        })));

        assert_eq!(behaviors.len(), 1);
        assert_eq!(behaviors[0].provider_kind, "");
        assert_eq!(behaviors[0].endpoint, "");
        assert_eq!(behaviors[0].context_window, None);
        // Missing `enabled` defaults to true.
        assert!(behaviors[0].enabled);
    }

    #[test]
    fn context_budget_counts_and_picks_latest_compaction() {
        let (_, budget) = build_self_view(envelope(json!({
            "AgentBehavior": [],
            "InferenceBackend": [],
            "InferenceProfile": [],
            "CompactionEntry": [
                { "created_at": "2026-06-01T10:00:00Z", "original_tokens": 100, "compacted_tokens": 40 },
                { "created_at": "2026-06-02T10:00:00Z", "original_tokens": 200, "compacted_tokens": 80 }
            ]
        })));

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
        let (_, budget) = build_self_view(envelope(json!({
            "AgentBehavior": [], "InferenceBackend": [], "InferenceProfile": [], "CompactionEntry": []
        })));
        assert_eq!(budget.compaction_count, 0);
        assert_eq!(budget.latest_compaction_at, None);
    }
}
