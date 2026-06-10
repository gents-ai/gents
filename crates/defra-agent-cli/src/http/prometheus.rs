use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::http::liveness::{
    compute_request_liveness_summary, with_active_native_executors, LivenessRequestRow,
    LivenessToolCallRow, RuntimeLivenessSnapshot,
};
use crate::post_graphql;

#[derive(Debug, Serialize)]
pub(crate) struct MetricsQueryData {
    pub(crate) agent_runtimes: Vec<MetricsRuntimeRow>,
    pub(crate) inference_backends: Vec<MetricsBackendRow>,
    pub(crate) liveness: RuntimeLivenessSnapshot,
}

#[derive(Debug, Deserialize)]
struct MetricsQueryEnvelope {
    #[serde(rename = "AgentRuntime", default)]
    agent_runtimes: Vec<MetricsRuntimeRow>,
    #[serde(rename = "InferenceBackend", default)]
    inference_backends: Vec<MetricsBackendRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<LivenessRequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    tool_calls: Vec<LivenessToolCallRow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct MetricsRuntimeRow {
    pub(crate) agent_did: String,
    #[serde(default)]
    pub(crate) process_state: String,
    #[serde(default)]
    pub(crate) reconcile_phase: String,
    #[serde(default)]
    pub(crate) active_generation: i64,
    #[serde(default)]
    pub(crate) router_generation: i64,
    #[serde(default)]
    pub(crate) runnable_behavior_count: i64,
    #[serde(default)]
    pub(crate) unavailable_behavior_count: i64,
    #[serde(default)]
    pub(crate) last_reconcile_result: String,
    #[serde(default)]
    pub(crate) last_reconcile_completed_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct MetricsBackendRow {
    pub(crate) backend_id: String,
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) max_concurrent: i64,
    #[serde(default)]
    pub(crate) max_queue_depth: i64,
    #[serde(default, deserialize_with = "null_string_as_unknown")]
    pub(crate) probe_status: String,
    pub(crate) last_probe: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InferenceMetricsQueryData {
    #[serde(rename = "AgentPrincipal", default)]
    principals: Vec<InferencePrincipalRow>,
    #[serde(rename = "AgentBehavior", default)]
    behaviors: Vec<InferenceBehaviorRow>,
    #[serde(rename = "InferenceCall", default)]
    calls: Vec<InferenceCallMetricRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct InferencePrincipalRow {
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceBehaviorRow {
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    backend_id: String,
    #[serde(default)]
    model_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceCallMetricRow {
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    behavior_id: String,
    #[serde(default)]
    backend_id: String,
    #[serde(default)]
    call_state: String,
    #[serde(default)]
    prompt_tokens: Option<i64>,
    #[serde(default)]
    completion_tokens: Option<i64>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct InferenceRequestMetricKey {
    agent: String,
    agent_did: String,
    backend_id: String,
    model: String,
    status: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct InferenceTokenMetricKey {
    agent: String,
    agent_did: String,
    backend_id: String,
    model: String,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct InferenceMetricFamilies {
    request_totals: BTreeMap<InferenceRequestMetricKey, i64>,
    prompt_token_totals: BTreeMap<InferenceTokenMetricKey, i64>,
    completion_token_totals: BTreeMap<InferenceTokenMetricKey, i64>,
}

fn null_string_as_unknown<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(|| "unknown".to_string()))
}

pub(crate) async fn render_prometheus_metrics(graphql: &str) -> Result<String> {
    let data = load_metrics_query_data(graphql).await?;
    let data = with_local_native_executors(data);
    let inference_metrics = load_inference_metrics_query_data(graphql).await?;

    let mut lines = Vec::new();
    push_metric_prelude(
        &mut lines,
        "defra_agent_up",
        "Whether the defra-agent process is serving.",
    );
    push_metric_sample(&mut lines, "defra_agent_up", &[], 1);

    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_process_state",
        "One-hot process lifecycle state for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_reconcile_phase",
        "One-hot reconcile phase for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_last_reconcile_result",
        "One-hot last reconcile result for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_active_generation",
        "Current active runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_router_generation",
        "Current router-observed runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_runnable_behaviors",
        "Number of runnable behaviors in the active runtime snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_unavailable_behaviors",
        "Number of unavailable behaviors in the active runtime snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_runtime_last_reconcile_completed_at_seconds",
        "Unix timestamp of the last completed reconcile.",
    );

    for runtime in &data.agent_runtimes {
        let agent_did = runtime.agent_did.clone();
        for state in [
            "uninitialized",
            "recovering",
            "ready",
            "shuttingDown",
            "shutdown",
        ] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_process_state",
                &[
                    ("agent_did", agent_did.clone()),
                    ("state", state.to_string()),
                ],
                i64::from(runtime.process_state == state),
            );
        }
        for phase in ["idle", "debouncing", "resolving", "diffing", "applying"] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_reconcile_phase",
                &[
                    ("agent_did", agent_did.clone()),
                    ("phase", phase.to_string()),
                ],
                i64::from(runtime.reconcile_phase == phase),
            );
        }
        for result in ["startup", "noop", "applied", "error"] {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_last_reconcile_result",
                &[
                    ("agent_did", agent_did.clone()),
                    ("result", result.to_string()),
                ],
                i64::from(runtime.last_reconcile_result == result),
            );
        }
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_active_generation",
            &[("agent_did", agent_did.clone())],
            runtime.active_generation,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_router_generation",
            &[("agent_did", agent_did.clone())],
            runtime.router_generation,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_runnable_behaviors",
            &[("agent_did", agent_did.clone())],
            runtime.runnable_behavior_count,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_runtime_unavailable_behaviors",
            &[("agent_did", agent_did.clone())],
            runtime.unavailable_behavior_count,
        );
        if let Some(timestamp) = rfc3339_timestamp_seconds(&runtime.last_reconcile_completed_at) {
            push_metric_sample(
                &mut lines,
                "defra_agent_runtime_last_reconcile_completed_at_seconds",
                &[("agent_did", agent_did)],
                timestamp,
            );
        }
    }

    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_enabled",
        "Whether an inference backend is enabled.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_max_concurrent",
        "Configured maximum concurrency for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_max_queue_depth",
        "Configured admission queue depth for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_probe_status",
        "Current probe status for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_backend_last_probe_seconds",
        "Unix timestamp of the last backend probe.",
    );

    for backend in &data.inference_backends {
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_enabled",
            &[("backend_id", backend.backend_id.clone())],
            i64::from(backend.enabled),
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_max_concurrent",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_concurrent,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_max_queue_depth",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_queue_depth,
        );
        push_metric_sample(
            &mut lines,
            "defra_agent_backend_probe_status",
            &[
                ("backend_id", backend.backend_id.clone()),
                ("status", backend.probe_status.clone()),
            ],
            1,
        );
        if let Some(timestamp) = backend
            .last_probe
            .as_deref()
            .and_then(rfc3339_timestamp_seconds)
        {
            push_metric_sample(
                &mut lines,
                "defra_agent_backend_last_probe_seconds",
                &[("backend_id", backend.backend_id.clone())],
                timestamp,
            );
        }
    }

    push_metric_prelude(
        &mut lines,
        "defra_agent_active_requests",
        "Number of AgentRequest rows currently in processing.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_active_requests",
        &[],
        data.liveness.active_request_ids.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_active_tool_calls",
        "Number of AgentToolCall rows currently running.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_active_tool_calls",
        &[],
        data.liveness.active_tool_calls.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_expired_processing_count",
        "Number of processing AgentRequest rows whose deadline has already passed. Zero is healthy.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_expired_processing_count",
        &[],
        data.liveness.expired_processing_count,
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_active_native_executors",
        "Number of active managed native executor processes visible in this HTTP server process.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_active_native_executors",
        &[],
        data.liveness.active_native_executors.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_active_native_executors_available",
        "Per-instance gauge set to 1 when active native executor process snapshots were collected from this HTTP server process; aggregate with min/max rather than average.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_active_native_executors_available",
        &[],
        if data.liveness.active_native_executors_available {
            1
        } else {
            0
        },
    );

    render_inference_metrics(&mut lines, &inference_metrics);

    lines.push(String::new());
    Ok(lines.join("\n"))
}

async fn load_inference_metrics_query_data(graphql: &str) -> Result<InferenceMetricsQueryData> {
    let response = post_graphql(graphql, inference_metrics_query()).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(data).context("decoding inference metrics query response")
}

fn inference_metrics_query() -> &'static str {
    r#"{
        AgentPrincipal {
            agent_did
            display_name
        }
        AgentBehavior {
            behavior_id
            agent_did
            display_name
            backend_id
            model_name
        }
        InferenceCall(filter: {
            call_kind: { _eq: "inference" },
            call_state: { _in: ["completed", "failed", "cancelled"] }
        }) {
            agent_did
            behavior_id
            backend_id
            call_state
            prompt_tokens
            completion_tokens
        }
    }"#
}

fn render_inference_metrics(lines: &mut Vec<String>, data: &InferenceMetricsQueryData) {
    let families = build_inference_metric_families(data);

    push_metric_counter_prelude(
        lines,
        "defra_agent_inference_requests_total",
        "Cumulative terminal inference calls grouped by agent, backend, model, and terminal status.",
    );
    for (key, total) in families.request_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_requests_total",
            &[
                ("agent", key.agent),
                ("agent_did", key.agent_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
                ("status", key.status),
            ],
            total,
        );
    }

    push_metric_counter_prelude(
        lines,
        "defra_agent_inference_prompt_tokens_total",
        "Cumulative prompt tokens reported by terminal inference calls.",
    );
    for (key, total) in families.prompt_token_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_prompt_tokens_total",
            &[
                ("agent", key.agent),
                ("agent_did", key.agent_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
            ],
            total,
        );
    }

    push_metric_counter_prelude(
        lines,
        "defra_agent_inference_completion_tokens_total",
        "Cumulative completion tokens reported by terminal inference calls.",
    );
    for (key, total) in families.completion_token_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_completion_tokens_total",
            &[
                ("agent", key.agent),
                ("agent_did", key.agent_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
            ],
            total,
        );
    }
}

fn build_inference_metric_families(data: &InferenceMetricsQueryData) -> InferenceMetricFamilies {
    let principals = data
        .principals
        .iter()
        .filter_map(|principal| {
            let agent_did = clean_metric_label(&principal.agent_did)?;
            Some((agent_did, clean_metric_label(&principal.display_name)))
        })
        .collect::<BTreeMap<_, _>>();
    let behaviors = data
        .behaviors
        .iter()
        .filter_map(|behavior| {
            let behavior_id = clean_metric_label(&behavior.behavior_id)?;
            Some((behavior_id, behavior))
        })
        .collect::<BTreeMap<_, _>>();

    let mut families = InferenceMetricFamilies::default();
    for call in &data.calls {
        if !is_terminal_inference_status(&call.call_state) {
            continue;
        }

        let behavior = clean_metric_label(&call.behavior_id)
            .as_deref()
            .and_then(|behavior_id| behaviors.get(behavior_id).copied());
        let agent_did = clean_metric_label(&call.agent_did)
            .or_else(|| behavior.and_then(|behavior| clean_metric_label(&behavior.agent_did)))
            .unwrap_or_else(|| "unknown".to_string());
        let agent = principals
            .get(&agent_did)
            .cloned()
            .flatten()
            .or_else(|| behavior.and_then(|behavior| clean_metric_label(&behavior.display_name)))
            .unwrap_or_else(|| agent_did.clone());
        let backend_id = clean_metric_label(&call.backend_id)
            .or_else(|| behavior.and_then(|behavior| clean_metric_label(&behavior.backend_id)))
            .unwrap_or_else(|| "unknown".to_string());
        let model = behavior
            .and_then(|behavior| clean_metric_label(&behavior.model_name))
            .unwrap_or_else(|| "unknown".to_string());
        let status = clean_metric_label(&call.call_state).unwrap_or_else(|| "unknown".to_string());

        let request_key = InferenceRequestMetricKey {
            agent: agent.clone(),
            agent_did: agent_did.clone(),
            backend_id: backend_id.clone(),
            model: model.clone(),
            status,
        };
        *families.request_totals.entry(request_key).or_default() += 1;

        let token_key = InferenceTokenMetricKey {
            agent,
            agent_did,
            backend_id,
            model,
        };
        if let Some(prompt_tokens) = nonnegative_metric_value(call.prompt_tokens) {
            *families
                .prompt_token_totals
                .entry(token_key.clone())
                .or_default() += prompt_tokens;
        }
        if let Some(completion_tokens) = nonnegative_metric_value(call.completion_tokens) {
            *families
                .completion_token_totals
                .entry(token_key)
                .or_default() += completion_tokens;
        }
    }

    families
}

fn is_terminal_inference_status(status: &str) -> bool {
    matches!(status.trim(), "completed" | "failed" | "cancelled")
}

fn clean_metric_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

fn nonnegative_metric_value(value: Option<i64>) -> Option<i64> {
    value.map(|value| value.max(0))
}

pub(crate) async fn load_metrics_query_data(graphql: &str) -> Result<MetricsQueryData> {
    let response = post_graphql(
        graphql,
        r#"{
            AgentRuntime {
                agent_did
                process_state
                reconcile_phase
                active_generation
                router_generation
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
                last_reconcile_completed_at
            }
            InferenceBackend {
                backend_id
                enabled
                max_concurrent
                max_queue_depth
                probe_status
                last_probe
            }
            AgentRequest(filter: {
                status: { _eq: "processing" },
                lifecycle_state: { _eq: "processing" }
            }) {
                request_id
                claimed_at
                deadline
                subagent_depth
                caused_by_parent_request_id
                caused_by_trigger_kind
            }
            AgentToolCall(filter: {
                lifecycle_state: { _eq: "running" }
            }) {
                request_id
                tool_call_id
                tool_name
                started_at
                deadline_at
                await_mode
            }
        }"#,
    )
    .await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let envelope: MetricsQueryEnvelope =
        serde_json::from_value(data).context("decoding runtime HTTP query response")?;
    let liveness =
        compute_request_liveness_summary(Utc::now(), envelope.requests, envelope.tool_calls);
    Ok(MetricsQueryData {
        agent_runtimes: envelope.agent_runtimes,
        inference_backends: envelope.inference_backends,
        liveness,
    })
}

pub(crate) fn with_local_native_executors(mut data: MetricsQueryData) -> MetricsQueryData {
    data.liveness =
        with_active_native_executors(data.liveness, defra_agent::active_native_executors());
    data
}

fn push_metric_prelude(lines: &mut Vec<String>, name: &str, help: &str) {
    push_metric_prelude_with_type(lines, name, help, "gauge");
}

fn push_metric_counter_prelude(lines: &mut Vec<String>, name: &str, help: &str) {
    push_metric_prelude_with_type(lines, name, help, "counter");
}

fn push_metric_prelude_with_type(lines: &mut Vec<String>, name: &str, help: &str, kind: &str) {
    lines.push(format!("# HELP {name} {help}"));
    lines.push(format!("# TYPE {name} {kind}"));
}

fn push_metric_sample(
    lines: &mut Vec<String>,
    name: &str,
    labels: &[(&str, String)],
    value: impl std::fmt::Display,
) {
    lines.push(format!("{name}{} {value}", format_metric_labels(labels),));
}

fn format_metric_labels(labels: &[(&str, String)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let rendered = labels
        .iter()
        .map(|(key, value)| format!(r#"{key}="{}""#, escape_prometheus_label(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{{{rendered}}}")
}

fn escape_prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

fn rfc3339_timestamp_seconds(value: &str) -> Option<i64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_query_envelope_treats_null_probe_status_as_unknown() {
        let envelope: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
            "AgentRuntime": [],
            "InferenceBackend": [{
                "backend_id": "workstation-1",
                "enabled": true,
                "max_concurrent": 2,
                "max_queue_depth": 8,
                "probe_status": null,
                "last_probe": null
            }],
            "AgentRequest": [],
            "AgentToolCall": []
        }))
        .expect("metrics query envelope should decode null probe_status");

        assert_eq!(envelope.inference_backends[0].probe_status, "unknown");
    }

    #[test]
    fn inference_metrics_group_by_agent_backend_model_and_status() {
        let data = InferenceMetricsQueryData {
            principals: vec![InferencePrincipalRow {
                agent_did: "did:key:zAgent".to_string(),
                display_name: "observability-steward".to_string(),
            }],
            behaviors: vec![InferenceBehaviorRow {
                behavior_id: "behavior-1".to_string(),
                agent_did: "did:key:zAgent".to_string(),
                display_name: "fallback-behavior-name".to_string(),
                backend_id: "backend-from-behavior".to_string(),
                model_name: "d4f".to_string(),
            }],
            calls: vec![
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "completed".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(4),
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "completed".to_string(),
                    prompt_tokens: Some(7),
                    completion_tokens: Some(3),
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "failed".to_string(),
                    prompt_tokens: Some(5),
                    completion_tokens: None,
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "running".to_string(),
                    prompt_tokens: Some(99),
                    completion_tokens: Some(99),
                },
            ],
        };

        let families = build_inference_metric_families(&data);
        let completed_key = InferenceRequestMetricKey {
            agent: "observability-steward".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            backend_id: "backend-1".to_string(),
            model: "d4f".to_string(),
            status: "completed".to_string(),
        };
        let failed_key = InferenceRequestMetricKey {
            status: "failed".to_string(),
            ..completed_key.clone()
        };
        let token_key = InferenceTokenMetricKey {
            agent: "observability-steward".to_string(),
            agent_did: "did:key:zAgent".to_string(),
            backend_id: "backend-1".to_string(),
            model: "d4f".to_string(),
        };

        assert_eq!(families.request_totals.get(&completed_key), Some(&2));
        assert_eq!(families.request_totals.get(&failed_key), Some(&1));
        assert_eq!(families.prompt_token_totals.get(&token_key), Some(&22));
        assert_eq!(families.completion_token_totals.get(&token_key), Some(&7));
    }

    #[test]
    fn render_inference_metrics_emits_counter_families() {
        let data = InferenceMetricsQueryData {
            principals: vec![],
            behaviors: vec![InferenceBehaviorRow {
                behavior_id: "behavior-1".to_string(),
                agent_did: "did:key:zAgent".to_string(),
                display_name: "agent \"friendly\"".to_string(),
                backend_id: "backend-1".to_string(),
                model_name: "model\none".to_string(),
            }],
            calls: vec![InferenceCallMetricRow {
                agent_did: String::new(),
                behavior_id: "behavior-1".to_string(),
                backend_id: String::new(),
                call_state: "completed".to_string(),
                prompt_tokens: Some(10),
                completion_tokens: Some(3),
            }],
        };

        let mut lines = Vec::new();
        render_inference_metrics(&mut lines, &data);
        let body = lines.join("\n");

        assert!(body.contains("# TYPE defra_agent_inference_requests_total counter"));
        assert!(body.contains(
            "defra_agent_inference_requests_total{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",status=\"completed\"} 1"
        ));
        assert!(body.contains(
            "defra_agent_inference_prompt_tokens_total{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 10"
        ));
        assert!(body.contains(
            "defra_agent_inference_completion_tokens_total{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 3"
        ));
    }
}
