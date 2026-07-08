use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::http::liveness::{
    compute_request_liveness_summary, with_active_native_executors, LivenessRequestRow,
    LivenessToolCallRow, RuntimeLivenessSnapshot,
};
use crate::post_graphql;

const INFERENCE_METRICS_WINDOW_SECS: i64 = 5 * 60;
const INFERENCE_METRICS_PAGE_SIZE: usize = 500;

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

#[derive(Debug)]
struct InferenceMetricsQueryData {
    principals: Vec<InferencePrincipalRow>,
    behaviors: Vec<InferenceBehaviorRow>,
    calls: Vec<InferenceCallMetricRow>,
    window_seconds: i64,
}

#[derive(Debug, Deserialize)]
struct InferenceMetricsPageData {
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
    ended_at: String,
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

pub(crate) async fn render_prometheus_metrics(
    graphql: &str,
    local_agent_did: &str,
    measured_backend_health: &HashMap<String, defra_agent::BackendHealthSnapshot>,
) -> Result<String> {
    let data = load_metrics_query_data(graphql, local_agent_did).await?;
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

    push_backend_metrics(
        &mut lines,
        &data.inference_backends,
        measured_backend_health,
    );

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
        "defra_agent_ignored_foreign_processing_requests",
        "Number of processing AgentRequest rows ignored because they belong to a different agent DID.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_ignored_foreign_processing_requests",
        &[],
        data.liveness.ignored_foreign_processing_count,
    );
    push_metric_prelude(
        &mut lines,
        "defra_agent_ignored_foreign_running_tool_calls",
        "Number of running AgentToolCall rows ignored because they belong to a different agent DID.",
    );
    push_metric_sample(
        &mut lines,
        "defra_agent_ignored_foreign_running_tool_calls",
        &[],
        data.liveness.ignored_foreign_tool_call_count,
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
    let window_started_at = Utc::now() - Duration::seconds(INFERENCE_METRICS_WINDOW_SECS);
    let mut result = InferenceMetricsQueryData {
        principals: Vec::new(),
        behaviors: Vec::new(),
        calls: Vec::new(),
        window_seconds: INFERENCE_METRICS_WINDOW_SECS,
    };
    let mut offset = 0usize;

    // InferenceCall timestamps are persisted as strings today, so the Rust
    // DefraDB GraphQL schema does not expose range filters for ended_at. Keep
    // the scrape bounded by paging newest terminal calls and stopping once the
    // ordered page crosses the local cutoff.
    loop {
        let query = inference_metrics_query(INFERENCE_METRICS_PAGE_SIZE, offset, offset == 0);
        let page = load_inference_metrics_page(graphql, &query).await?;
        if offset == 0 {
            result.principals = page.principals;
            result.behaviors = page.behaviors;
        }

        let page_len = page.calls.len();
        let (recent_calls, reached_window_start) =
            retain_windowed_inference_calls(page.calls, &window_started_at);
        result.calls.extend(recent_calls);

        if page_len < INFERENCE_METRICS_PAGE_SIZE || reached_window_start {
            break;
        }
        offset += INFERENCE_METRICS_PAGE_SIZE;
    }

    Ok(result)
}

async fn load_inference_metrics_page(
    graphql: &str,
    query: &str,
) -> Result<InferenceMetricsPageData> {
    let response = post_graphql(graphql, query).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(data).context("decoding inference metrics query response")
}

fn inference_metrics_query(limit: usize, offset: usize, include_metadata: bool) -> String {
    let metadata = if include_metadata {
        r#"
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
"#
    } else {
        ""
    };

    format!(
        r#"{{
        {metadata}
        InferenceCall(
            filter: {{
                call_kind: {{ _eq: "inference" }},
                call_state: {{ _in: ["completed", "failed", "cancelled"] }}
            }},
            order: [{{ ended_at: DESC }}, {{ call_id: DESC }}],
            limit: {limit},
            offset: {offset}
        ) {{
            agent_did
            behavior_id
            backend_id
            call_state
            ended_at
            prompt_tokens
            completion_tokens
        }}
    }}"#
    )
}

fn render_inference_metrics(lines: &mut Vec<String>, data: &InferenceMetricsQueryData) {
    let families = build_inference_metric_families(data);

    push_metric_prelude(
        lines,
        "defra_agent_inference_metrics_window_seconds",
        "Trailing scrape window, in seconds, used for inference request and token gauges.",
    );
    push_metric_sample(
        lines,
        "defra_agent_inference_metrics_window_seconds",
        &[],
        data.window_seconds,
    );

    push_metric_prelude(
        lines,
        "defra_agent_inference_requests_window_count",
        "Terminal inference calls ended inside the trailing scrape window, grouped by agent, backend, model, and terminal status; this gauge is not cumulative.",
    );
    for (key, total) in families.request_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_requests_window_count",
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

    push_metric_prelude(
        lines,
        "defra_agent_inference_prompt_tokens_window_sum",
        "Prompt tokens reported by terminal inference calls ended inside the trailing scrape window; this gauge is not cumulative.",
    );
    for (key, total) in families.prompt_token_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_prompt_tokens_window_sum",
            &[
                ("agent", key.agent),
                ("agent_did", key.agent_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
            ],
            total,
        );
    }

    push_metric_prelude(
        lines,
        "defra_agent_inference_completion_tokens_window_sum",
        "Completion tokens reported by terminal inference calls ended inside the trailing scrape window; this gauge is not cumulative.",
    );
    for (key, total) in families.completion_token_totals {
        push_metric_sample(
            lines,
            "defra_agent_inference_completion_tokens_window_sum",
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

fn retain_windowed_inference_calls(
    calls: Vec<InferenceCallMetricRow>,
    window_started_at: &DateTime<Utc>,
) -> (Vec<InferenceCallMetricRow>, bool) {
    let mut recent = Vec::new();
    let mut reached_window_start = false;
    for call in calls {
        let Some(ended_at) = rfc3339_timestamp(&call.ended_at) else {
            reached_window_start = true;
            continue;
        };
        if ended_at < *window_started_at {
            reached_window_start = true;
        } else {
            recent.push(call);
        }
    }
    (recent, reached_window_start)
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

pub(crate) async fn load_metrics_query_data(
    graphql: &str,
    local_agent_did: &str,
) -> Result<MetricsQueryData> {
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
                agent_did
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
                agent_did
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
    let liveness = compute_request_liveness_summary(
        Utc::now(),
        local_agent_did,
        envelope.requests,
        envelope.tool_calls,
    );
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

/// Per-backend metric samples with the local runtime's measured probe
/// health overlaid (#640). When the prober has measured a backend, the
/// `status` label carries the MEASURED state and the sample value is 1 iff
/// that state is healthy — so `defra_agent_backend_probe_status` genuinely
/// reads 0 for a dead endpoint instead of pinning at the stored document
/// constant. Backends the prober never measures (ChatGPT-Codex, or an HTTP
/// surface without an in-process runtime) fall back to the document's
/// `probe_status`/`last_probe`.
pub(crate) fn push_backend_metrics(
    lines: &mut Vec<String>,
    backends: &[MetricsBackendRow],
    measured: &HashMap<String, defra_agent::BackendHealthSnapshot>,
) {
    for backend in backends {
        push_metric_sample(
            lines,
            "defra_agent_backend_enabled",
            &[("backend_id", backend.backend_id.clone())],
            i64::from(backend.enabled),
        );
        push_metric_sample(
            lines,
            "defra_agent_backend_max_concurrent",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_concurrent,
        );
        push_metric_sample(
            lines,
            "defra_agent_backend_max_queue_depth",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_queue_depth,
        );
        let measured_entry = measured.get(&backend.backend_id);
        let status = measured_entry
            .map(|entry| entry.state.as_str().to_string())
            .unwrap_or_else(|| backend.probe_status.clone());
        push_metric_sample(
            lines,
            "defra_agent_backend_probe_status",
            &[
                ("backend_id", backend.backend_id.clone()),
                ("status", status.clone()),
            ],
            i64::from(status == "healthy"),
        );
        let last_probe_seconds = measured_entry
            .map(|entry| entry.last_probe_at.timestamp())
            .or_else(|| {
                backend
                    .last_probe
                    .as_deref()
                    .and_then(rfc3339_timestamp_seconds)
            });
        if let Some(timestamp) = last_probe_seconds {
            push_metric_sample(
                lines,
                "defra_agent_backend_last_probe_seconds",
                &[("backend_id", backend.backend_id.clone())],
                timestamp,
            );
        }
    }
}

fn push_metric_prelude(lines: &mut Vec<String>, name: &str, help: &str) {
    push_metric_prelude_with_type(lines, name, help, "gauge");
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
    rfc3339_timestamp(value).map(|timestamp| timestamp.timestamp())
}

fn rfc3339_timestamp(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|timestamp| timestamp.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #640 ledger consumer (`backend-health.operatorCli`): the probe-status
    /// metric must report the local runtime's MEASURED health — value 0 with
    /// the measured status label for a dead endpoint, fresh last_probe from
    /// the measurement, doc fallback only where the prober has no opinion.
    #[test]
    fn backend_probe_status_metric_reflects_measured_health() {
        fn doc_row(
            backend_id: &str,
            probe_status: &str,
            last_probe: Option<&str>,
        ) -> MetricsBackendRow {
            MetricsBackendRow {
                backend_id: backend_id.to_string(),
                enabled: true,
                max_concurrent: 4,
                max_queue_depth: 100,
                probe_status: probe_status.to_string(),
                last_probe: last_probe.map(str::to_string),
            }
        }
        fn measured_entry(
            backend_id: &str,
            state: defra_agent::BackendHealthState,
            failure_count: u32,
            last_probe_at: &str,
        ) -> defra_agent::BackendHealthSnapshot {
            defra_agent::BackendHealthSnapshot {
                backend_id: backend_id.to_string(),
                state,
                failure_count,
                last_probe_at: chrono::DateTime::parse_from_rfc3339(last_probe_at)
                    .unwrap()
                    .with_timezone(&Utc),
                last_error: (failure_count > 0).then(|| "connection refused".to_string()),
            }
        }

        let backends = vec![
            // The fleet-evidence shape: document pinned "healthy", endpoint
            // measured dead — the metric MUST read 0.
            doc_row("sparks-cluster", "healthy", None),
            // Measured healthy: 1 with a fresh measured last_probe.
            doc_row("workstation-1", "healthy", Some("2026-07-01T00:00:00Z")),
            // Below-threshold blip: truthful degraded label, value 0.
            doc_row("spark-2", "healthy", None),
            // Never probed (e.g. ChatGPT-Codex): document status governs.
            doc_row("codex", "healthy", Some("2026-07-06T00:00:00Z")),
            doc_row("unprobed-unknown", "unknown", None),
        ];
        let measured = HashMap::from([
            (
                "sparks-cluster".to_string(),
                measured_entry(
                    "sparks-cluster",
                    defra_agent::BackendHealthState::Unhealthy,
                    3,
                    "2026-07-07T12:00:00Z",
                ),
            ),
            (
                "workstation-1".to_string(),
                measured_entry(
                    "workstation-1",
                    defra_agent::BackendHealthState::Healthy,
                    0,
                    "2026-07-07T12:00:00Z",
                ),
            ),
            (
                "spark-2".to_string(),
                measured_entry(
                    "spark-2",
                    defra_agent::BackendHealthState::Degraded,
                    1,
                    "2026-07-07T12:00:00Z",
                ),
            ),
        ]);

        let mut lines = Vec::new();
        push_backend_metrics(&mut lines, &backends, &measured);
        let rendered = lines.join("\n");

        let measured_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-07T12:00:00Z")
            .unwrap()
            .timestamp();
        assert!(rendered.contains(
            r#"defra_agent_backend_probe_status{backend_id="sparks-cluster",status="unhealthy"} 0"#
        ));
        assert!(rendered.contains(&format!(
            r#"defra_agent_backend_last_probe_seconds{{backend_id="sparks-cluster"}} {measured_last_probe}"#
        )));
        assert!(rendered.contains(
            r#"defra_agent_backend_probe_status{backend_id="workstation-1",status="healthy"} 1"#
        ));
        assert!(rendered.contains(&format!(
            r#"defra_agent_backend_last_probe_seconds{{backend_id="workstation-1"}} {measured_last_probe}"#
        )));
        assert!(rendered.contains(
            r#"defra_agent_backend_probe_status{backend_id="spark-2",status="degraded"} 0"#
        ));
        // Doc fallback: measured absent.
        assert!(rendered.contains(
            r#"defra_agent_backend_probe_status{backend_id="codex",status="healthy"} 1"#
        ));
        let doc_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-06T00:00:00Z")
            .unwrap()
            .timestamp();
        assert!(rendered.contains(&format!(
            r#"defra_agent_backend_last_probe_seconds{{backend_id="codex"}} {doc_last_probe}"#
        )));
        assert!(rendered.contains(
            r#"defra_agent_backend_probe_status{backend_id="unprobed-unknown",status="unknown"} 0"#
        ));
        // No last_probe series at all when neither measurement nor doc has one.
        assert!(!rendered
            .contains(r#"defra_agent_backend_last_probe_seconds{backend_id="unprobed-unknown"}"#));
    }

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
                    ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
                    prompt_tokens: Some(10),
                    completion_tokens: Some(4),
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "completed".to_string(),
                    ended_at: "2026-06-13T10:00:01.000000000+00:00".to_string(),
                    prompt_tokens: Some(7),
                    completion_tokens: Some(3),
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "failed".to_string(),
                    ended_at: "2026-06-13T10:00:02.000000000+00:00".to_string(),
                    prompt_tokens: Some(5),
                    completion_tokens: None,
                },
                InferenceCallMetricRow {
                    agent_did: "did:key:zAgent".to_string(),
                    behavior_id: "behavior-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "running".to_string(),
                    ended_at: "2026-06-13T10:00:03.000000000+00:00".to_string(),
                    prompt_tokens: Some(99),
                    completion_tokens: Some(99),
                },
            ],
            window_seconds: INFERENCE_METRICS_WINDOW_SECS,
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
    fn inference_metrics_query_pages_terminal_calls_by_newest_end_time() {
        let first_page = inference_metrics_query(500, 0, true);
        assert!(first_page.contains("AgentPrincipal"));
        assert!(first_page.contains("AgentBehavior"));
        assert!(first_page.contains("order: [{ ended_at: DESC }, { call_id: DESC }]"));
        assert!(first_page.contains("limit: 500"));
        assert!(first_page.contains("offset: 0"));

        let later_page = inference_metrics_query(500, 500, false);
        assert!(!later_page.contains("AgentPrincipal"));
        assert!(!later_page.contains("AgentBehavior"));
        assert!(later_page.contains("offset: 500"));
    }

    #[test]
    fn retain_windowed_inference_calls_keeps_recent_and_stops_on_old_rows() {
        let window_started_at = rfc3339_timestamp("2026-06-13T10:00:00.000000000+00:00").unwrap();
        let calls = vec![
            InferenceCallMetricRow {
                agent_did: String::new(),
                behavior_id: String::new(),
                backend_id: String::new(),
                call_state: "completed".to_string(),
                ended_at: "2026-06-13T10:00:01.000000000+00:00".to_string(),
                prompt_tokens: None,
                completion_tokens: None,
            },
            InferenceCallMetricRow {
                agent_did: String::new(),
                behavior_id: String::new(),
                backend_id: String::new(),
                call_state: "completed".to_string(),
                ended_at: "2026-06-13T09:59:59.999999999+00:00".to_string(),
                prompt_tokens: None,
                completion_tokens: None,
            },
        ];

        let (recent, reached_window_start) =
            retain_windowed_inference_calls(calls, &window_started_at);

        assert_eq!(recent.len(), 1);
        assert!(reached_window_start);
        assert_eq!(recent[0].ended_at, "2026-06-13T10:00:01.000000000+00:00");
    }

    #[test]
    fn render_inference_metrics_emits_windowed_gauge_families() {
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
                ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
                prompt_tokens: Some(10),
                completion_tokens: Some(3),
            }],
            window_seconds: INFERENCE_METRICS_WINDOW_SECS,
        };

        let mut lines = Vec::new();
        render_inference_metrics(&mut lines, &data);
        let body = lines.join("\n");

        assert!(body.contains("# TYPE defra_agent_inference_metrics_window_seconds gauge"));
        assert!(body.contains("defra_agent_inference_metrics_window_seconds 300"));
        assert!(body.contains("# TYPE defra_agent_inference_requests_window_count gauge"));
        assert!(body.contains(
            "defra_agent_inference_requests_window_count{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",status=\"completed\"} 1"
        ));
        assert!(body.contains(
            "defra_agent_inference_prompt_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 10"
        ));
        assert!(body.contains(
            "defra_agent_inference_completion_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 3"
        ));
    }
}
