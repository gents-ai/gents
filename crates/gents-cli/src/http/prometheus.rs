use gents::store::config_client::GraphqlEndpoint;
use rapidhash::RapidHashMap;
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use gents_protocol::row::{
    decode_node_readiness_snapshot, project_node_readiness_summary, NodeReadinessRow,
    ProjectedNodeReadinessSummary,
};
use gents_protocol::usage::{TokenUsageColumns, TokenUsageSummary};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::http::liveness::{
    compute_request_liveness_summary, owns_liveness_row, with_active_native_executors,
    LivenessActivityRow, LivenessRequestRow, LivenessToolCallRow, RuntimeLivenessSnapshot,
};
use crate::post_graphql;
use gents::store::graphql::escape_graphql_string;

mod p2p_sync;
mod process;

pub(crate) use process::{sample_process_memory, ProcessMetricsSnapshot, StoreSizeCache};

const INFERENCE_METRICS_WINDOW_SECS: i64 = 5 * 60;
const INFERENCE_METRICS_PAGE_SIZE: usize = 500;
const LIVENESS_ACTIVITY_CHUNK: usize = 32;
pub(crate) const LIVENESS_ACTIVITY_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(500);

#[derive(Debug, Serialize)]
pub(crate) struct MetricsQueryData {
    pub(crate) node_runtimes: Vec<MetricsRuntimeRow>,
    pub(crate) node_readiness: Vec<NodeReadinessRow>,
    pub(crate) inference_backends: Vec<MetricsBackendRow>,
    pub(crate) liveness: RuntimeLivenessSnapshot,
}

#[derive(Debug, Deserialize)]
struct MetricsQueryEnvelope {
    #[serde(rename = "NodeRuntime", default)]
    node_runtimes: Vec<MetricsRuntimeRow>,
    #[serde(rename = "NodeReadiness", default)]
    node_readiness: Vec<NodeReadinessRow>,
    #[serde(rename = "InferenceBackend", default)]
    inference_backends: Vec<MetricsBackendRow>,
    #[serde(rename = "AgentRequest", default)]
    requests: Vec<LivenessRequestRow>,
    #[serde(rename = "AgentToolCall", default)]
    tool_calls: Vec<LivenessToolCallRow>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct MetricsRuntimeRow {
    pub(crate) node_did: String,
    #[serde(default)]
    pub(crate) reconcile_phase: String,
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
    pub(crate) max_concurrent: Option<i64>,
    #[serde(default)]
    pub(crate) max_queue_depth: Option<i64>,
    #[serde(default, deserialize_with = "null_string_as_unknown")]
    pub(crate) probe_status: String,
    pub(crate) last_probe: Option<String>,
}

#[derive(Debug)]
struct InferenceMetricsQueryData {
    nodes: Vec<InferenceNodeRow>,
    agents: Vec<InferenceAgentRow>,
    calls: Vec<InferenceCallMetricRow>,
    window_seconds: i64,
}

#[derive(Debug, Deserialize)]
struct InferenceMetricsPageData {
    #[serde(rename = "Node", default)]
    nodes: Vec<InferenceNodeRow>,
    #[serde(rename = "Agent", default)]
    agents: Vec<InferenceAgentRow>,
    #[serde(rename = "InferenceCall", default)]
    calls: Vec<InferenceCallMetricRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceNodeRow {
    #[serde(default)]
    node_did: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceAgentRow {
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    node_did: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InferenceCallMetricRow {
    #[serde(default)]
    node_did: String,
    #[serde(default)]
    agent_id: String,
    #[serde(default)]
    backend_id: String,
    #[serde(default)]
    call_state: String,
    #[serde(default)]
    ended_at: String,
    #[serde(default, deserialize_with = "null_string_as_unknown")]
    model_name: String,
    #[serde(default)]
    call_kind: String,
    #[serde(flatten)]
    usage: TokenUsageColumns,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct InferenceRequestMetricKey {
    agent: String,
    node_did: String,
    backend_id: String,
    model: String,
    status: String,
    call_kind: String,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct InferenceTokenMetricKey {
    agent: String,
    node_did: String,
    backend_id: String,
    model: String,
    call_kind: String,
}

#[derive(Debug, Default, Eq, PartialEq)]
struct InferenceMetricFamilies {
    request_totals: BTreeMap<InferenceRequestMetricKey, i64>,
    usage: BTreeMap<InferenceTokenMetricKey, Option<TokenUsageSummary>>,
}

fn null_string_as_unknown<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_else(|| "unknown".to_string()))
}

#[derive(Debug, Clone, Default)]
pub(crate) struct P2pMetricsSnapshot {
    pub(crate) enabled: bool,
    pub(crate) connected_peers: usize,
    pub(crate) replicators: usize,
    pub(crate) admission: Option<crate::shared::P2pAdmissionState>,
    pub(crate) sync_status: Option<gents::P2pSyncStatusSnapshot>,
    pub(crate) stale: bool,
    pub(crate) peer_id: Option<String>,
    pub(crate) transport: Option<String>,
}

pub(crate) async fn render_prometheus_metrics(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
    measured_backend_health: &RapidHashMap<String, gents::BackendHealthSnapshot>,
    p2p: Option<&P2pMetricsSnapshot>,
    process: Option<&ProcessMetricsSnapshot>,
) -> Result<String> {
    let data = load_metrics_query_data(graphql, local_node_did).await?;
    let data = with_local_native_executors(data);
    let inference_metrics = load_inference_metrics_query_data(graphql).await?;
    let background_completion = gents::load_background_completion_diagnostics(
        &gents::store::config_client::ConfigAccess::Graphql(graphql.clone()),
        local_node_did,
    )
    .await
    .ok();

    let mut lines = Vec::new();
    push_metric_prelude(
        &mut lines,
        "gents_up",
        "Whether the gents process is serving.",
    );
    push_metric_sample(&mut lines, "gents_up", &[], 1);

    push_metric_prelude(
        &mut lines,
        "gents_runtime_process_state",
        "One-hot process lifecycle state for each node runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_reconcile_phase",
        "One-hot reconcile phase for each node runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_last_reconcile_result",
        "One-hot last reconcile result for each node runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_active_generation",
        "Current active runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_router_generation",
        "Current router-observed runtime generation.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_runnable_agents",
        "Number of ready agents in the authoritative agent-readiness snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_unavailable_agents",
        "Number of unavailable agents in the authoritative agent-readiness snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_node_readiness_observed",
        "Whether the authoritative agent-readiness snapshot is valid and current.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_last_reconcile_completed_at_seconds",
        "Unix timestamp of the last completed reconcile.",
    );

    for readiness in runtime_inventory(&data) {
        let node_did = readiness.node_did.clone();
        let runtime = data
            .node_runtimes
            .iter()
            .find(|runtime| runtime.node_did == node_did);
        let lifecycle = decode_node_readiness_snapshot(readiness, &node_did).ok();
        for state in [
            "uninitialized",
            "recovering",
            "ready",
            "shuttingDown",
            "shutdown",
        ] {
            push_metric_sample(
                &mut lines,
                "gents_runtime_process_state",
                &[("node_did", node_did.clone()), ("state", state.to_string())],
                i64::from(
                    lifecycle
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.process_state.as_str() == state),
                ),
            );
        }
        if let Some(runtime) = runtime {
            for phase in ["idle", "debouncing", "resolving", "diffing", "applying"] {
                push_metric_sample(
                    &mut lines,
                    "gents_runtime_reconcile_phase",
                    &[("node_did", node_did.clone()), ("phase", phase.to_string())],
                    i64::from(runtime.reconcile_phase == phase),
                );
            }
            for result in ["startup", "noop", "applied", "error"] {
                push_metric_sample(
                    &mut lines,
                    "gents_runtime_last_reconcile_result",
                    &[
                        ("node_did", node_did.clone()),
                        ("result", result.to_string()),
                    ],
                    i64::from(runtime.last_reconcile_result == result),
                );
            }
        }
        push_metric_sample(
            &mut lines,
            "gents_runtime_active_generation",
            &[("node_did", node_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.active_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_metric_sample(
            &mut lines,
            "gents_runtime_router_generation",
            &[("node_did", node_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.router_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_node_readiness_metrics(&mut lines, &node_did, &data.node_readiness);
        if let Some(timestamp) = runtime
            .and_then(|runtime| rfc3339_timestamp_seconds(&runtime.last_reconcile_completed_at))
        {
            push_metric_sample(
                &mut lines,
                "gents_runtime_last_reconcile_completed_at_seconds",
                &[("node_did", node_did)],
                timestamp,
            );
        }
    }

    push_metric_prelude(
        &mut lines,
        "gents_backend_enabled",
        "Whether an inference backend is enabled.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_backend_max_concurrent",
        "Configured maximum concurrency for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_backend_max_queue_depth",
        "Configured admission queue depth for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_backend_probe_status",
        "Current probe status for an inference backend.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_backend_last_probe_seconds",
        "Unix timestamp of the last backend probe.",
    );

    push_backend_metrics(
        &mut lines,
        &data.inference_backends,
        measured_backend_health,
    );

    push_metric_prelude(
        &mut lines,
        "gents_active_requests",
        "Number of AgentRequest rows currently in processing.",
    );
    push_metric_sample(
        &mut lines,
        "gents_active_requests",
        &[],
        data.liveness.active_request_ids.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "gents_active_tool_calls",
        "Number of AgentToolCall rows currently running.",
    );
    push_metric_sample(
        &mut lines,
        "gents_active_tool_calls",
        &[],
        data.liveness.active_tool_calls.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "gents_expired_processing_count",
        "Number of processing AgentRequest rows whose deadline has already passed. Zero is healthy.",
    );
    push_metric_sample(
        &mut lines,
        "gents_expired_processing_count",
        &[],
        data.liveness.expired_processing_count,
    );
    push_metric_prelude(
        &mut lines,
        "gents_ignored_foreign_processing_requests",
        "Number of processing AgentRequest rows ignored because they belong to a different node DID.",
    );
    push_metric_sample(
        &mut lines,
        "gents_ignored_foreign_processing_requests",
        &[],
        data.liveness.ignored_foreign_processing_count,
    );
    push_metric_prelude(
        &mut lines,
        "gents_ignored_foreign_running_tool_calls",
        "Number of running AgentToolCall rows ignored because they belong to a different node DID.",
    );
    push_metric_sample(
        &mut lines,
        "gents_ignored_foreign_running_tool_calls",
        &[],
        data.liveness.ignored_foreign_tool_call_count,
    );
    push_metric_prelude(
        &mut lines,
        "gents_active_native_executors",
        "Number of active managed native executor processes visible in this HTTP server process.",
    );
    push_metric_sample(
        &mut lines,
        "gents_active_native_executors",
        &[],
        data.liveness.active_native_executors.len() as i64,
    );
    push_metric_prelude(
        &mut lines,
        "gents_active_native_executors_available",
        "Per-instance gauge set to 1 when active native executor process snapshots were collected from this HTTP server process; aggregate with min/max rather than average.",
    );
    push_metric_sample(
        &mut lines,
        "gents_active_native_executors_available",
        &[],
        if data.liveness.active_native_executors_available {
            1
        } else {
            0
        },
    );

    render_inference_metrics(&mut lines, &inference_metrics);
    render_command_policy_metrics(
        &mut lines,
        gents::tools::native::command_observability::snapshot(),
    );
    render_background_completion_metrics(
        &mut lines,
        local_node_did,
        background_completion.as_ref(),
    );
    render_p2p_metrics(&mut lines, p2p);
    process::render_process_metrics(&mut lines, process);

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn render_command_policy_metrics(
    lines: &mut Vec<String>,
    snapshot: gents::tools::native::command_observability::CommandPolicyMetrics,
) {
    for (name, help, value) in [
        (
            "gents_command_policy_rejections_total",
            "Command policy decisions that refused a launch or guard operation in this process.",
            snapshot.policy_rejections,
        ),
        (
            "gents_command_credential_inspection_failures_total",
            "Failed inspections of protected credential namespaces in this process.",
            snapshot.credential_inspection_failures,
        ),
    ] {
        push_metric_prelude_with_type(lines, name, help, "counter");
        push_metric_sample(lines, name, &[], value);
    }
}

fn runtime_inventory(data: &MetricsQueryData) -> impl Iterator<Item = &NodeReadinessRow> {
    data.node_readiness.iter()
}

fn push_node_readiness_metrics(lines: &mut Vec<String>, node_did: &str, rows: &[NodeReadinessRow]) {
    push_node_readiness_metrics_at(lines, node_did, rows, chrono::Utc::now());
}

fn push_node_readiness_metrics_at(
    lines: &mut Vec<String>,
    node_did: &str,
    rows: &[NodeReadinessRow],
    _observed_at: chrono::DateTime<chrono::Utc>,
) {
    let readiness = rows.iter().find(|row| row.node_did == node_did);
    let (ready, unavailable, observed) = match project_node_readiness_summary(readiness, node_did) {
        ProjectedNodeReadinessSummary::Observed(summary) => (
            i64::try_from(summary.ready_count).unwrap_or(i64::MAX),
            i64::try_from(summary.unavailable_agents.len()).unwrap_or(i64::MAX),
            1,
        ),
        ProjectedNodeReadinessSummary::Unknown(_) => (0, 0, 0),
    };
    let labels = [("node_did", node_did.to_string())];
    push_metric_sample(lines, "gents_runtime_runnable_agents", &labels, ready);
    push_metric_sample(
        lines,
        "gents_runtime_unavailable_agents",
        &labels,
        unavailable,
    );
    push_metric_sample(
        lines,
        "gents_runtime_node_readiness_observed",
        &labels,
        observed,
    );
}

fn render_background_completion_metrics(
    lines: &mut Vec<String>,
    node_did: &str,
    diagnostics: Option<&gents::BackgroundCompletionDiagnostics>,
) {
    let labels = [("node_did", node_did.to_string())];
    push_metric_prelude(
        lines,
        "gents_background_completion_diagnostics_available",
        "1 when durable background-completion delivery diagnostics were available to this scrape.",
    );
    push_metric_sample(
        lines,
        "gents_background_completion_diagnostics_available",
        &labels,
        i64::from(diagnostics.is_some()),
    );
    let Some(diagnostics) = diagnostics else {
        return;
    };
    for (name, help, value) in [
        (
            "gents_background_completion_pending_notifications",
            "Durable background-completion notifications not yet acknowledged by a successful continuation snapshot.",
            diagnostics.pending_notifications as i64,
        ),
        (
            "gents_background_completion_acknowledged_notifications",
            "Durable background-completion notifications acknowledged by a successful continuation snapshot.",
            diagnostics.acknowledged_notifications as i64,
        ),
        (
            "gents_background_completion_stranded_notifications",
            "Unacknowledged background-completion notifications with no retryable continuation remaining.",
            diagnostics.stranded_notifications as i64,
        ),
        (
            "gents_background_completion_oldest_pending_age_seconds",
            "Age in seconds of the oldest unacknowledged background-completion notification epoch.",
            diagnostics.oldest_pending_age_seconds.unwrap_or(0),
        ),
        (
            "gents_background_completion_scan_truncated",
            "1 when the bounded durable diagnostics scan reached its wake or notification row limit.",
            i64::from(diagnostics.scan_truncated),
        ),
    ] {
        push_metric_prelude(lines, name, help);
        push_metric_sample(lines, name, &labels, value);
    }
    push_metric_prelude(
        lines,
        "gents_background_completion_epochs",
        "Background-completion continuation epochs grouped by durable delivery state.",
    );
    let mut epochs_by_state = BTreeMap::<&str, i64>::new();
    for epoch in &diagnostics.epochs {
        *epochs_by_state.entry(epoch.state.as_str()).or_default() += 1;
    }
    for (state, count) in epochs_by_state {
        push_metric_sample(
            lines,
            "gents_background_completion_epochs",
            &[
                ("node_did", node_did.to_string()),
                ("state", state.to_string()),
            ],
            count,
        );
    }
}

fn render_p2p_metrics(lines: &mut Vec<String>, p2p: Option<&P2pMetricsSnapshot>) {
    let snapshot = p2p.cloned().unwrap_or_default();

    push_metric_prelude(
        lines,
        "gents_p2p_enabled",
        "1 when this process has P2P transport enabled.",
    );
    push_metric_sample(
        lines,
        "gents_p2p_enabled",
        &[],
        if snapshot.enabled { 1 } else { 0 },
    );

    push_metric_prelude(
        lines,
        "gents_p2p_status_stale",
        "1 when live P2P state is last-known or admission-only because the self-fetch timed out or failed.",
    );
    push_metric_sample(
        lines,
        "gents_p2p_status_stale",
        &[],
        if snapshot.stale { 1 } else { 0 },
    );

    push_metric_prelude(
        lines,
        "gents_p2p_connected_peers",
        "Connected P2P peers (last successful live fetch; held when status_stale=1).",
    );
    push_metric_sample(
        lines,
        "gents_p2p_connected_peers",
        &[],
        snapshot.connected_peers as i64,
    );

    push_metric_prelude(
        lines,
        "gents_p2p_replicators",
        "Configured P2P replicators (last successful live fetch; held when status_stale=1).",
    );
    push_metric_sample(
        lines,
        "gents_p2p_replicators",
        &[],
        snapshot.replicators as i64,
    );

    p2p_sync::render_self_info(
        lines,
        snapshot.peer_id.as_deref(),
        snapshot.transport.as_deref().unwrap_or_default(),
    );

    render_p2p_sync_metrics(lines, snapshot.sync_status.as_ref());
    p2p_sync::render_sync_diagnostics(lines, snapshot.sync_status.as_ref(), Utc::now());

    push_metric_prelude(
        lines,
        "gents_p2p_admission_max_pending_dags",
        "Requested max pending-DAG registrations at serve start (not the effective post-clamp bound).",
    );
    push_metric_prelude(
        lines,
        "gents_p2p_admission_max_concurrent_push_tasks",
        "Requested max concurrent outbound PushLog worker slots at serve start.",
    );
    push_metric_prelude(
        lines,
        "gents_p2p_admission_max_concurrent_dag_fetches",
        "Requested max concurrent Bitswap DAG fetches at serve start.",
    );
    push_metric_prelude(
        lines,
        "gents_p2p_admission_rate_limit_burst",
        "Requested per-peer P2P rate-limit burst at serve start.",
    );
    push_metric_prelude(
        lines,
        "gents_p2p_admission_rate_limit_rate",
        "Requested per-peer P2P rate-limit refill rate (tokens/s) at serve start; upstream may floor below 1.0.",
    );

    if let Some(admission) = snapshot.admission.as_ref() {
        push_metric_sample(
            lines,
            "gents_p2p_admission_max_pending_dags",
            &[],
            admission.max_pending_dags as i64,
        );
        push_metric_sample(
            lines,
            "gents_p2p_admission_max_concurrent_push_tasks",
            &[],
            admission.max_concurrent_push_tasks as i64,
        );
        push_metric_sample(
            lines,
            "gents_p2p_admission_max_concurrent_dag_fetches",
            &[],
            admission.max_concurrent_dag_fetches as i64,
        );
        push_metric_sample(
            lines,
            "gents_p2p_admission_rate_limit_burst",
            &[],
            admission.rate_limit_burst as i64,
        );
        push_metric_sample(
            lines,
            "gents_p2p_admission_rate_limit_rate",
            &[],
            admission.rate_limit_rate,
        );
    } else {
        for name in [
            "gents_p2p_admission_max_pending_dags",
            "gents_p2p_admission_max_concurrent_push_tasks",
            "gents_p2p_admission_max_concurrent_dag_fetches",
            "gents_p2p_admission_rate_limit_burst",
            "gents_p2p_admission_rate_limit_rate",
        ] {
            push_metric_sample(lines, name, &[], 0);
        }
    }
}

fn render_p2p_sync_metrics(lines: &mut Vec<String>, status: Option<&gents::P2pSyncStatusSnapshot>) {
    push_metric_prelude(
        lines,
        "gents_p2p_sync_status_available",
        "1 when the pinned DefraDB live sync diagnostics snapshot was fetched and decoded.",
    );
    push_metric_sample(
        lines,
        "gents_p2p_sync_status_available",
        &[],
        i64::from(status.is_some()),
    );

    let status = status.cloned().unwrap_or_default();
    let backlog = &status.push_backlog;
    for (name, help, value) in [
        (
            "gents_p2p_push_queue_items",
            "Live outbound push jobs waiting in the bounded DefraDB queue.",
            backlog.queued_items,
        ),
        (
            "gents_p2p_push_queue_item_capacity",
            "Effective outbound push queue item capacity in DefraDB.",
            backlog.queue_item_capacity,
        ),
        (
            "gents_p2p_push_queue_bytes",
            "Accounted bytes held by live outbound push jobs waiting in DefraDB.",
            backlog.queued_bytes,
        ),
        (
            "gents_p2p_push_queue_byte_capacity",
            "Effective outbound push queue byte capacity in DefraDB.",
            backlog.queue_byte_capacity,
        ),
        (
            "gents_p2p_push_active_jobs",
            "Outbound push jobs currently owned by DefraDB workers.",
            backlog.active_jobs,
        ),
        (
            "gents_p2p_push_worker_count",
            "Fixed outbound push worker count in DefraDB.",
            backlog.worker_count,
        ),
        (
            "gents_p2p_push_per_peer_active_cap",
            "Effective maximum active outbound push jobs for one peer.",
            backlog.per_peer_active_cap,
        ),
        (
            "gents_p2p_pending_dags",
            "Live in-memory pending DAG registrations.",
            status.pending_dags,
        ),
        (
            "gents_p2p_pending_dag_capacity",
            "Effective in-memory pending DAG capacity.",
            status.pending_dag_capacity,
        ),
        (
            "gents_p2p_persisted_pending_dags",
            "Durable pending DAG registrations awaiting merge or retirement.",
            status.persisted_pending_dags,
        ),
        (
            "gents_p2p_persisted_pending_dag_capacity",
            "Effective durable pending DAG registration capacity.",
            status.persisted_pending_dag_capacity,
        ),
        (
            "gents_p2p_quarantined_pending_dags",
            "Pending DAG roots quarantined after a terminal merge rejection.",
            status.quarantined_pending_dags,
        ),
        (
            "gents_p2p_retained_background_tasks",
            "DefraDB P2P background task handles retained for shutdown.",
            status.retained_background_tasks,
        ),
    ] {
        push_metric_prelude(lines, name, help);
        push_metric_sample(lines, name, &[], value as i64);
    }

    push_metric_prelude(
        lines,
        "gents_p2p_pending_resync_in_flight",
        "1 when DefraDB is reconciling durable pending DAG registrations.",
    );
    push_metric_sample(
        lines,
        "gents_p2p_pending_resync_in_flight",
        &[],
        i64::from(status.pending_resync_in_flight),
    );

    for (name, help, value) in [
        (
            "gents_p2p_push_enqueued_total",
            "Outbound push jobs admitted by the bounded DefraDB queue.",
            backlog.enqueued_total,
        ),
        (
            "gents_p2p_push_coalesced_total",
            "Duplicate outbound push jobs coalesced by DefraDB.",
            backlog.coalesced_total,
        ),
        (
            "gents_p2p_push_rejected_items_total",
            "Outbound push admissions rejected at the item cap.",
            backlog.rejected_items_total,
        ),
        (
            "gents_p2p_push_rejected_bytes_total",
            "Outbound push admissions rejected at the byte cap.",
            backlog.rejected_bytes_total,
        ),
        (
            "gents_p2p_push_completed_total",
            "Outbound push jobs completed by DefraDB workers.",
            backlog.completed_total,
        ),
        (
            "gents_p2p_push_failed_total",
            "Outbound push jobs failed by DefraDB workers.",
            backlog.failed_total,
        ),
        (
            "gents_p2p_push_stale_head_retirements_total",
            "Stale outbound CID heads retired by DefraDB push coalescing.",
            backlog.stale_head_retirements_total,
        ),
        (
            "gents_p2p_push_peer_capacity_parks_total",
            "Times a peer was parked because its receiver reported saturation.",
            backlog.peer_capacity_parks_total,
        ),
        (
            "gents_p2p_broadcast_coalesced_total",
            "Redundant DefraDB broadcast updates coalesced before outbound push.",
            status.broadcast_coalesced_total,
        ),
        (
            "gents_p2p_push_updates_coalesced_total",
            "Outbound push updates retired in favor of a newer CID head.",
            status.push_updates_coalesced_total,
        ),
        (
            "gents_p2p_missing_link_retries_total",
            "Missing-link retry attempts recorded by DefraDB sync.",
            status.missing_link_retries,
        ),
        (
            "gents_p2p_pending_dag_resolved_total",
            "Pending DAG registrations resolved by DefraDB sync.",
            status.pending_dag_resolved,
        ),
        (
            "gents_p2p_pending_dag_expired_total",
            "Pending DAG registrations expired by DefraDB sync.",
            status.pending_dag_expired,
        ),
        (
            "gents_p2p_gossip_direction_filtered_total",
            "Inbound gossip dropped by the replication-direction filter.",
            status.gossip_direction_filtered_total,
        ),
        (
            "gents_p2p_pending_dag_capacity_shed_total",
            "Inbound roots shed because the pending-DAG registry was at capacity.",
            status.pending_dag_capacity_shed,
        ),
        (
            "gents_p2p_pending_dag_fetch_deferred_unavailable_total",
            "Due pending DAG fetches deferred because no qualified provider was connected.",
            status.pending_dag_fetch_deferred_unavailable,
        ),
        (
            "gents_p2p_pending_dag_fetch_deferred_contention_total",
            "Useful CAR ingests deferred behind an existing local storage owner.",
            status.pending_dag_fetch_deferred_contention,
        ),
        (
            "gents_p2p_pending_dag_fetch_deferred_busy_total",
            "Due pending DAG fetches deferred because every provider answered busy.",
            status.pending_dag_fetch_deferred_busy,
        ),
        (
            "gents_p2p_single_flight_suppressed_total",
            "Duplicate concurrent receives of the same CID suppressed by DefraDB sync.",
            status.single_flight_suppressed,
        ),
        (
            "gents_p2p_already_merged_fast_path_total",
            "Receives skipped because the announced CID was already merged.",
            status.already_merged_fast_path,
        ),
        (
            "gents_p2p_pending_dag_terminal_quarantined_total",
            "Pending DAG roots quarantined after a deterministic terminal merge rejection.",
            status.pending_dag_terminal_quarantined,
        ),
    ] {
        push_metric_prelude_with_type(lines, name, help, "counter");
        push_metric_sample(lines, name, &[], value);
    }

    for (name, help) in [
        (
            "gents_p2p_peer_push_queue_items",
            "Live outbound push jobs waiting for one peer.",
        ),
        (
            "gents_p2p_peer_push_queue_bytes",
            "Accounted outbound push queue bytes held for one peer.",
        ),
        (
            "gents_p2p_peer_push_active_jobs",
            "Outbound push jobs currently active for one peer.",
        ),
        (
            "gents_p2p_peer_consecutive_failures",
            "Consecutive outbound push failures recorded for one peer.",
        ),
        (
            "gents_p2p_peer_cooldown_remaining_milliseconds",
            "Remaining outbound push cooldown for one peer, in milliseconds.",
        ),
    ] {
        push_metric_prelude(lines, name, help);
    }
    for peer in &backlog.per_peer {
        let labels = &[("peer_id", peer.peer_id.clone())];
        for (name, value) in [
            ("gents_p2p_peer_push_queue_items", peer.queued_items as u64),
            ("gents_p2p_peer_push_queue_bytes", peer.queued_bytes as u64),
            ("gents_p2p_peer_push_active_jobs", peer.active_jobs as u64),
            (
                "gents_p2p_peer_consecutive_failures",
                peer.consecutive_failures as u64,
            ),
            (
                "gents_p2p_peer_cooldown_remaining_milliseconds",
                peer.cooldown_remaining_ms,
            ),
        ] {
            push_metric_sample(lines, name, labels, value);
        }
    }
}

async fn load_inference_metrics_query_data(
    graphql: &GraphqlEndpoint,
) -> Result<InferenceMetricsQueryData> {
    let window_started_at = Utc::now() - Duration::seconds(INFERENCE_METRICS_WINDOW_SECS);
    let mut result = InferenceMetricsQueryData {
        nodes: Vec::new(),
        agents: Vec::new(),
        calls: Vec::new(),
        window_seconds: INFERENCE_METRICS_WINDOW_SECS,
    };
    let mut offset = 0usize;

    loop {
        let query = inference_metrics_query(
            INFERENCE_METRICS_PAGE_SIZE,
            offset,
            offset == 0,
            &window_started_at,
        );
        let page = load_inference_metrics_page(graphql, &query).await?;
        if offset == 0 {
            result.nodes = page.nodes;
            result.agents = page.agents;
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
    graphql: &GraphqlEndpoint,
    query: &str,
) -> Result<InferenceMetricsPageData> {
    let response = post_graphql(graphql, query).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    serde_json::from_value(data).context("decoding inference metrics query response")
}

/// `ended_at` is stored as RFC 3339 UTC text. The bound keeps whole seconds
/// and no offset, so it sorts before every timestamp inside the window; rows
/// are still checked exactly by `retain_windowed_inference_calls`.
fn inference_metrics_ended_lower_bound(window_started_at: &DateTime<Utc>) -> String {
    window_started_at.format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn inference_metrics_query(
    limit: usize,
    offset: usize,
    include_metadata: bool,
    window_started_at: &DateTime<Utc>,
) -> String {
    let ended_after = inference_metrics_ended_lower_bound(window_started_at);
    let metadata = if include_metadata {
        r#"
        Node {
            node_did
            display_name
        }
        Agent {
            agent_id
            node_did
            display_name
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
                call_kind: {{ _in: ["inference", "compaction", "scheduled", "oneoff"] }},
                call_state: {{ _in: ["completed", "failed", "cancelled"] }},
                ended_at: {{ _ge: "{ended_after}" }}
            }},
            order: [{{ ended_at: DESC }}, {{ call_id: DESC }}],
            limit: {limit},
            offset: {offset}
        ) {{
            node_did
            agent_id
            backend_id
            call_state
            ended_at
            model_name
            call_kind
            prompt_tokens
            completion_tokens
            cached_input_tokens
            cache_creation_input_tokens
            reasoning_output_tokens
            provider_total_tokens
            usage_completeness
        }}
    }}"#
    )
}

fn render_inference_metrics(lines: &mut Vec<String>, data: &InferenceMetricsQueryData) {
    let families = build_inference_metric_families(data);

    push_metric_prelude(
        lines,
        "gents_inference_metrics_window_seconds",
        "Trailing scrape window, in seconds, used for inference request and token gauges.",
    );
    push_metric_sample(
        lines,
        "gents_inference_metrics_window_seconds",
        &[],
        data.window_seconds,
    );

    push_metric_prelude(
        lines,
        "gents_inference_requests_window_count",
        "Terminal inference calls ended inside the trailing scrape window, grouped by agent, backend, model, and terminal status; this gauge is not cumulative.",
    );
    for (key, total) in families.request_totals {
        push_metric_sample(
            lines,
            "gents_inference_requests_window_count",
            &[
                ("agent", key.agent),
                ("node_did", key.node_did),
                ("backend_id", key.backend_id),
                ("model", key.model),
                ("status", key.status),
                ("call_kind", key.call_kind),
            ],
            total,
        );
    }

    type Counter = fn(&TokenUsageSummary) -> Option<u64>;
    let counters: &[(&str, &str, Counter)] = &[
        (
            "prompt_tokens",
            "Reported input tokens, including cache reads and writes.",
            |s| s.input.observed(),
        ),
        (
            "completion_tokens",
            "Reported output tokens, including reasoning.",
            |s| s.output.observed(),
        ),
        (
            "cached_input_tokens",
            "Reported cache read tokens, a subset of input.",
            |s| s.cache_read.observed(),
        ),
        (
            "cache_creation_input_tokens",
            "Reported cache write tokens, a subset of input.",
            |s| s.cache_write.observed(),
        ),
        (
            "reasoning_output_tokens",
            "Reported reasoning tokens, a subset of output.",
            |s| s.reasoning.observed(),
        ),
        (
            "provider_total_tokens",
            "Provider-reported total evidence, not an additional charge.",
            |s| s.provider_total.observed(),
        ),
        (
            "incomplete_usage",
            "Calls without complete valid provider input and output usage.",
            |s| Some(s.incomplete_calls),
        ),
        (
            "input_reported_calls",
            "Calls reporting input tokens.",
            |s| Some(s.input.reported),
        ),
        (
            "output_reported_calls",
            "Calls reporting output tokens.",
            |s| Some(s.output.reported),
        ),
        (
            "cache_read_reported_calls",
            "Calls reporting cache read tokens.",
            |s| Some(s.cache_read.reported),
        ),
        (
            "cache_write_reported_calls",
            "Calls reporting cache write tokens.",
            |s| Some(s.cache_write.reported),
        ),
        (
            "reasoning_reported_calls",
            "Calls reporting reasoning tokens.",
            |s| Some(s.reasoning.reported),
        ),
        (
            "provider_total_reported_calls",
            "Calls reporting provider total tokens.",
            |s| Some(s.provider_total.reported),
        ),
    ];
    for (counter, help, value) in counters {
        let name = format!("gents_inference_{counter}_window_sum");
        push_metric_prelude(lines, &name, help);
        for (key, summary) in &families.usage {
            let Some(amount) = summary.as_ref().and_then(value) else {
                continue;
            };
            push_metric_sample(
                lines,
                &name,
                &[
                    ("agent", key.agent.clone()),
                    ("node_did", key.node_did.clone()),
                    ("backend_id", key.backend_id.clone()),
                    ("model", key.model.clone()),
                    ("call_kind", key.call_kind.clone()),
                ],
                amount,
            );
        }
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
    let nodes = data
        .nodes
        .iter()
        .filter_map(|principal| {
            let node_did = clean_metric_label(&principal.node_did)?;
            Some((node_did, clean_metric_label(&principal.display_name)))
        })
        .collect::<BTreeMap<_, _>>();
    let mut families = InferenceMetricFamilies::default();
    let mut observations = BTreeMap::<InferenceTokenMetricKey, Vec<_>>::new();
    for call in &data.calls {
        if !is_terminal_inference_status(&call.call_state) {
            continue;
        }

        let agent_id = clean_metric_label(&call.agent_id);
        let call_node_did = clean_metric_label(&call.node_did);
        let agent_config = agent_id.as_deref().and_then(|agent_id| {
            data.agents.iter().find(|agent_config| {
                clean_metric_label(&agent_config.agent_id).as_deref() == Some(agent_id)
                    && call_node_did
                        .as_deref()
                        .is_none_or(|node_did| agent_config.node_did == node_did)
            })
        });
        let node_did = call_node_did
            .or_else(|| {
                agent_config.and_then(|agent_config| clean_metric_label(&agent_config.node_did))
            })
            .unwrap_or_else(|| "unknown".to_string());
        let agent = nodes
            .get(&node_did)
            .cloned()
            .flatten()
            .or_else(|| {
                agent_config.and_then(|agent_config| clean_metric_label(&agent_config.display_name))
            })
            .unwrap_or_else(|| node_did.clone());
        let backend_id =
            clean_metric_label(&call.backend_id).unwrap_or_else(|| "unknown".to_string());
        let model = clean_metric_label(&call.model_name).unwrap_or_else(|| "unknown".to_string());
        let call_kind =
            clean_metric_label(&call.call_kind).unwrap_or_else(|| "unknown".to_string());
        let status = clean_metric_label(&call.call_state).unwrap_or_else(|| "unknown".to_string());

        let request_key = InferenceRequestMetricKey {
            agent: agent.clone(),
            node_did: node_did.clone(),
            backend_id: backend_id.clone(),
            model: model.clone(),
            status,
            call_kind: call_kind.clone(),
        };
        *families.request_totals.entry(request_key).or_default() += 1;

        let token_key = InferenceTokenMetricKey {
            agent,
            node_did,
            backend_id,
            model,
            call_kind,
        };
        observations
            .entry(token_key)
            .or_default()
            .push(call.usage.observation().unwrap_or_default());
    }

    families.usage = observations
        .into_iter()
        .map(|(key, values)| (key, TokenUsageSummary::from_observations(values).ok()))
        .collect();
    families
}

fn is_terminal_inference_status(status: &str) -> bool {
    matches!(status.trim(), "failed" | "completed" | "cancelled")
}

fn clean_metric_label(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

pub(crate) struct MetricsCoreData {
    envelope: MetricsQueryEnvelope,
    local_node_did: String,
}

pub(crate) async fn load_metrics_query_data(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
) -> Result<MetricsQueryData> {
    let core = load_metrics_core_data(graphql, local_node_did).await?;
    Ok(core
        .with_liveness_activity(
            graphql,
            tokio::time::Instant::now() + LIVENESS_ACTIVITY_BUDGET,
        )
        .await)
}

pub(crate) async fn load_metrics_core_data(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
) -> Result<MetricsCoreData> {
    let response = post_graphql(
        graphql,
        r#"{
            NodeRuntime {
                node_did
                reconcile_phase
                last_reconcile_result
                last_reconcile_completed_at
            }
            NodeReadiness {
                node_did
                snapshot_json
                updated_at
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
                lifecycle_state: { _eq: "processing" }
            }) {
                _docID
                request_id
                node_did
                claimed_at
                deadline
                request_hop
                caused_by_parent_request_id
                caused_by_trigger_kind
            }
            AgentToolCall(filter: {
                lifecycle_state: { _eq: "running" }
            }) {
                request_id
                node_did
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
    Ok(MetricsCoreData {
        envelope,
        local_node_did: local_node_did.to_string(),
    })
}

impl MetricsCoreData {
    pub(crate) async fn with_liveness_activity(
        self,
        graphql: &GraphqlEndpoint,
        deadline: tokio::time::Instant,
    ) -> MetricsQueryData {
        let Self {
            envelope,
            local_node_did,
        } = self;
        let deadline = deadline.min(tokio::time::Instant::now() + LIVENESS_ACTIVITY_BUDGET);
        let activity =
            load_liveness_activity(graphql, &local_node_did, &envelope.requests, deadline).await;
        let liveness = compute_request_liveness_summary(
            Utc::now(),
            &local_node_did,
            envelope.requests,
            envelope.tool_calls,
            activity,
        );
        MetricsQueryData {
            node_runtimes: envelope.node_runtimes,
            node_readiness: envelope.node_readiness,
            inference_backends: envelope.inference_backends,
            liveness,
        }
    }
}

async fn load_liveness_activity(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
    requests: &[LivenessRequestRow],
    deadline: tokio::time::Instant,
) -> Vec<LivenessActivityRow> {
    let request_doc_ids = requests
        .iter()
        .filter(|row| {
            owns_liveness_row(
                local_node_did.trim(),
                row.node_did.as_deref().unwrap_or_default(),
            )
        })
        .filter_map(|row| {
            let doc_id = row.doc_id.as_deref().map(str::trim)?;
            let node_did = row.node_did.as_deref().map(str::trim)?;
            (!doc_id.is_empty() && !node_did.is_empty()).then_some((doc_id, node_did))
        })
        .collect::<Vec<_>>();
    let mut activity = Vec::new();
    let mut chunks = request_doc_ids.chunks(LIVENESS_ACTIVITY_CHUNK);
    while let Some(chunk) = chunks.next() {
        match tokio::time::timeout_at(deadline, load_liveness_activity_chunk(graphql, chunk)).await
        {
            Ok(Ok(rows)) => activity.extend(rows),
            Ok(Err(error)) => tracing::warn!(
                request_count = chunk.len(),
                error = format!("{error:#}"),
                "liveness activity read failed; progress falls back to claimed_at"
            ),
            Err(_) => {
                let request_count = chunk.len() + chunks.map(<[_]>::len).sum::<usize>();
                tracing::warn!(
                    request_count,
                    budget_ms = LIVENESS_ACTIVITY_BUDGET.as_millis() as u64,
                    "liveness activity budget exhausted; progress falls back to claimed_at"
                );
                break;
            }
        }
    }
    activity
}

async fn load_liveness_activity_chunk(
    graphql: &GraphqlEndpoint,
    requests: &[(&str, &str)],
) -> Result<Vec<LivenessActivityRow>> {
    let response = post_graphql(graphql, &liveness_activity_query(requests)).await?;
    let data = response
        .get("data")
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    let pages: BTreeMap<String, Vec<LivenessActivityRow>> =
        serde_json::from_value(data).context("decoding liveness activity response")?;
    Ok(pages.into_values().flatten().collect())
}

fn liveness_activity_query(requests: &[(&str, &str)]) -> String {
    let mut selections = String::new();
    for (index, (request_doc_id, node_did)) in requests.iter().enumerate() {
        let request_doc_id = escape_graphql_string(request_doc_id);
        let node_did = escape_graphql_string(node_did);
        for (alias, collection, field, fields) in [
            (
                "ts",
                "AgentToolCall",
                "started_at",
                "started_at completed_at",
            ),
            (
                "tc",
                "AgentToolCall",
                "completed_at",
                "started_at completed_at",
            ),
            ("is", "InferenceCall", "started_at", "started_at ended_at"),
            ("ie", "InferenceCall", "ended_at", "started_at ended_at"),
        ] {
            selections.push_str(&format!(
                r#"
            {alias}{index}: {collection}(
                filter: {{
                    request_doc_id: {{ _eq: "{request_doc_id}" }},
                    node_did: {{ _eq: "{node_did}" }},
                    {field}: {{ _ne: null }}
                }},
                order: {{ {field}: DESC }},
                limit: 1
            ) {{
                request_doc_id
                node_did
                {fields}
            }}"#
            ));
        }
    }
    format!("{{{selections}\n}}")
}

pub(crate) fn with_local_native_executors(mut data: MetricsQueryData) -> MetricsQueryData {
    data.liveness = with_active_native_executors(data.liveness, gents::active_native_executors());
    data
}

pub(crate) fn push_backend_metrics(
    lines: &mut Vec<String>,
    backends: &[MetricsBackendRow],
    measured: &RapidHashMap<String, gents::BackendHealthSnapshot>,
) {
    for backend in backends {
        let observation = measured
            .get(&backend.backend_id)
            .map(|entry| (entry.state, entry.last_probe_at.timestamp()));
        push_backend_metric(lines, backend, observation);
    }
}

fn push_backend_metric(
    lines: &mut Vec<String>,
    backend: &MetricsBackendRow,
    measured: Option<(gents::BackendHealthState, i64)>,
) {
    push_metric_sample(
        lines,
        "gents_backend_enabled",
        &[("backend_id", backend.backend_id.clone())],
        i64::from(backend.enabled),
    );
    push_metric_sample(
        lines,
        "gents_backend_max_concurrent",
        &[("backend_id", backend.backend_id.clone())],
        backend
            .max_concurrent
            .unwrap_or(gents::inference::backends::registry::DEFAULT_MAX_CONCURRENT),
    );
    push_metric_sample(
        lines,
        "gents_backend_max_queue_depth",
        &[("backend_id", backend.backend_id.clone())],
        backend
            .max_queue_depth
            .unwrap_or(gents::inference::backends::registry::DEFAULT_MAX_QUEUE_DEPTH),
    );
    let status = measured
        .map(|(state, _)| state.as_str().to_string())
        .unwrap_or_else(|| backend.probe_status.clone());
    push_metric_sample(
        lines,
        "gents_backend_probe_status",
        &[
            ("backend_id", backend.backend_id.clone()),
            ("status", status.clone()),
        ],
        i64::from(status == "healthy"),
    );
    let last_probe_seconds = measured.map(|(_, timestamp)| timestamp).or_else(|| {
        backend
            .last_probe
            .as_deref()
            .and_then(rfc3339_timestamp_seconds)
    });
    if let Some(timestamp) = last_probe_seconds {
        push_metric_sample(
            lines,
            "gents_backend_last_probe_seconds",
            &[("backend_id", backend.backend_id.clone())],
            timestamp,
        );
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

    #[test]
    fn command_policy_metrics_expose_process_counts() {
        let mut lines = Vec::new();
        render_command_policy_metrics(
            &mut lines,
            gents::tools::native::command_observability::CommandPolicyMetrics {
                policy_rejections: 17,
                credential_inspection_failures: 5,
            },
        );
        let rendered = lines.join("\n");
        assert!(rendered.contains("# TYPE gents_command_policy_rejections_total counter"));
        assert!(rendered.contains("gents_command_policy_rejections_total 17"));
        assert!(
            rendered.contains("# TYPE gents_command_credential_inspection_failures_total counter")
        );
        assert!(rendered.contains("gents_command_credential_inspection_failures_total 5"));
    }

    #[test]
    fn backend_capacity_metrics_resolve_nullable_config_through_backend_defaults() {
        use gents::inference::backends::registry::{
            DEFAULT_MAX_CONCURRENT, DEFAULT_MAX_QUEUE_DEPTH,
        };

        for (limits, concurrency, queue_depth) in [
            (
                serde_json::json!({}),
                DEFAULT_MAX_CONCURRENT,
                DEFAULT_MAX_QUEUE_DEPTH,
            ),
            (
                serde_json::json!({"max_concurrent": null, "max_queue_depth": null}),
                DEFAULT_MAX_CONCURRENT,
                DEFAULT_MAX_QUEUE_DEPTH,
            ),
            (
                serde_json::json!({"max_concurrent": 7, "max_queue_depth": 0}),
                7,
                0,
            ),
        ] {
            let mut backend = serde_json::json!({
                "backend_id": "backend", "enabled": true, "probe_status": null
            });
            backend
                .as_object_mut()
                .unwrap()
                .extend(limits.as_object().unwrap().clone());
            let data: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
                "InferenceBackend": [backend]
            }))
            .unwrap();
            let mut lines = Vec::new();
            push_backend_metrics(
                &mut lines,
                &data.inference_backends,
                &RapidHashMap::default(),
            );
            assert!(lines.contains(&format!(
                "gents_backend_max_concurrent{{backend_id=\"backend\"}} {concurrency}"
            )));
            assert!(lines.contains(&format!(
                "gents_backend_max_queue_depth{{backend_id=\"backend\"}} {queue_depth}"
            )));
        }
    }

    #[tokio::test]
    async fn failed_liveness_activity_read_degrades_to_claimed_at() {
        use axum::{http::StatusCode, routing::post, Json, Router};

        let claimed_at = (Utc::now() - Duration::seconds(120)).to_rfc3339();
        let base = serde_json::json!({
            "data": {
                "NodeRuntime": [],
                "NodeReadiness": [],
                "InferenceBackend": [],
                "AgentRequest": [{
                    "_docID": "doc-req-1",
                    "request_id": "req-1",
                    "node_did": "did:test:local",
                    "claimed_at": claimed_at,
                }],
                "AgentToolCall": []
            }
        });
        let router = Router::new().route(
            "/api/v0/graphql",
            post(move |Json(body): Json<Value>| {
                let base = base.clone();
                async move {
                    let query = body["query"].as_str().unwrap_or_default().to_string();
                    if query.contains("NodeRuntime") {
                        Ok(Json(base))
                    } else {
                        Err(StatusCode::BAD_REQUEST)
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock graphql");
        let addr = listener.local_addr().expect("mock addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let data = load_metrics_query_data(
            &gents::store::config_client::GraphqlEndpoint::anonymous(format!(
                "http://{addr}/api/v0/graphql"
            )),
            "did:test:local",
        )
        .await
        .expect("an activity read failure must not fail the liveness owner");
        let request = &data.liveness.requests[0];
        assert_eq!(request.request_id, "req-1");
        assert!(
            (120_000..180_000).contains(&request.last_progress_age_ms),
            "progress falls back to claimed_at: {}",
            request.last_progress_age_ms
        );
    }

    use gents_protocol::row::{
        AgentReadinessEntry, AgentReadinessState, AgentReadinessUnavailableReason,
        NodeReadinessProcessState, NodeReadinessSnapshot, NODE_READINESS_FORMAT_VERSION,
    };

    fn metrics_runtime(node_did: &str) -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            node_did: node_did.to_string(),
            reconcile_phase: "idle".to_string(),
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-08-29T00:00:00Z".to_string(),
        }
    }

    fn metrics_readiness(node_did: &str) -> NodeReadinessRow {
        NodeReadinessRow {
            node_did: node_did.to_string(),
            snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
                format_version: NODE_READINESS_FORMAT_VERSION,
                process_state: NodeReadinessProcessState::Ready,
                active_generation: 4,
                router_generation: 4,
                default_agent_id: "default".to_string(),
                agents: vec![
                    AgentReadinessEntry {
                        agent_id: "default".to_string(),
                        state: AgentReadinessState::Ready,
                        reason: None,
                    },
                    AgentReadinessEntry {
                        agent_id: "offline".to_string(),
                        state: AgentReadinessState::Unavailable,
                        reason: Some(
                            AgentReadinessUnavailableReason::BackendTemporarilyUnavailable,
                        ),
                    },
                ],
            })
            .unwrap(),
            updated_at: "2026-08-29T00:00:00Z".to_string(),
        }
    }

    fn readiness_observed_at() -> chrono::DateTime<chrono::Utc> {
        "2026-08-29T00:00:30Z"
            .parse()
            .expect("fixed metrics observation timestamp must parse")
    }

    #[test]
    fn node_readiness_metrics_distinguish_observed_from_unknown() {
        let runtime = metrics_runtime("did:test:metrics");
        let mut lines = Vec::new();
        push_node_readiness_metrics_at(
            &mut lines,
            &runtime.node_did,
            &[metrics_readiness("did:test:metrics")],
            readiness_observed_at(),
        );
        let rendered = lines.join("\n");
        assert!(
            rendered.contains(r#"gents_runtime_runnable_agents{node_did="did:test:metrics"} 1"#)
        );
        assert!(
            rendered.contains(r#"gents_runtime_unavailable_agents{node_did="did:test:metrics"} 1"#)
        );
        assert!(rendered
            .contains(r#"gents_runtime_node_readiness_observed{node_did="did:test:metrics"} 1"#));

        for rows in [
            Vec::new(),
            vec![NodeReadinessRow {
                snapshot_json: "{}".to_string(),
                ..metrics_readiness("did:test:metrics")
            }],
        ] {
            let mut lines = Vec::new();
            push_node_readiness_metrics_at(
                &mut lines,
                &runtime.node_did,
                &rows,
                readiness_observed_at(),
            );
            let rendered = lines.join("\n");
            assert!(rendered
                .contains(r#"gents_runtime_runnable_agents{node_did="did:test:metrics"} 0"#));
            assert!(rendered
                .contains(r#"gents_runtime_unavailable_agents{node_did="did:test:metrics"} 0"#));
            assert!(rendered.contains(
                r#"gents_runtime_node_readiness_observed{node_did="did:test:metrics"} 0"#
            ));
        }
    }

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
                max_concurrent: Some(4),
                max_queue_depth: Some(100),
                probe_status: probe_status.to_string(),
                last_probe: last_probe.map(str::to_string),
            }
        }
        fn measured_entry(
            state: gents::BackendHealthState,
            last_probe_at: &str,
        ) -> (gents::BackendHealthState, i64) {
            (
                state,
                chrono::DateTime::parse_from_rfc3339(last_probe_at)
                    .unwrap()
                    .timestamp(),
            )
        }

        let backends = vec![
            doc_row("sparks-cluster", "healthy", None),
            doc_row("workstation-1", "healthy", Some("2026-07-01T00:00:00Z")),
            doc_row("spark-2", "healthy", None),
            doc_row("codex", "healthy", Some("2026-07-06T00:00:00Z")),
            doc_row("unprobed-unknown", "unknown", None),
        ];
        let measured = RapidHashMap::from_iter([
            (
                "sparks-cluster".to_string(),
                measured_entry(gents::BackendHealthState::Unhealthy, "2026-07-07T12:00:00Z"),
            ),
            (
                "workstation-1".to_string(),
                measured_entry(gents::BackendHealthState::Healthy, "2026-07-07T12:00:00Z"),
            ),
            (
                "spark-2".to_string(),
                measured_entry(gents::BackendHealthState::Degraded, "2026-07-07T12:00:00Z"),
            ),
        ]);

        let mut lines = Vec::new();
        for backend in &backends {
            push_backend_metric(
                &mut lines,
                backend,
                measured.get(&backend.backend_id).copied(),
            );
        }
        let rendered = lines.join("\n");

        let measured_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-07T12:00:00Z")
            .unwrap()
            .timestamp();
        assert!(rendered.contains(
            r#"gents_backend_probe_status{backend_id="sparks-cluster",status="unhealthy"} 0"#
        ));
        assert!(rendered.contains(&format!(
            r#"gents_backend_last_probe_seconds{{backend_id="sparks-cluster"}} {measured_last_probe}"#
        )));
        assert!(rendered.contains(
            r#"gents_backend_probe_status{backend_id="workstation-1",status="healthy"} 1"#
        ));
        assert!(rendered.contains(&format!(
            r#"gents_backend_last_probe_seconds{{backend_id="workstation-1"}} {measured_last_probe}"#
        )));
        assert!(rendered
            .contains(r#"gents_backend_probe_status{backend_id="spark-2",status="degraded"} 0"#));
        assert!(rendered
            .contains(r#"gents_backend_probe_status{backend_id="codex",status="healthy"} 1"#));
        let doc_last_probe = chrono::DateTime::parse_from_rfc3339("2026-07-06T00:00:00Z")
            .unwrap()
            .timestamp();
        assert!(rendered.contains(&format!(
            r#"gents_backend_last_probe_seconds{{backend_id="codex"}} {doc_last_probe}"#
        )));
        assert!(rendered.contains(
            r#"gents_backend_probe_status{backend_id="unprobed-unknown",status="unknown"} 0"#
        ));
        assert!(!rendered
            .contains(r#"gents_backend_last_probe_seconds{backend_id="unprobed-unknown"}"#));
    }

    #[test]
    fn background_completion_metrics_surface_pending_and_stranded_delivery() {
        let mut lines = Vec::new();
        render_background_completion_metrics(
            &mut lines,
            "did:agent:amy",
            Some(&gents::BackgroundCompletionDiagnostics {
                scanned_wakes: 1,
                scanned_notifications: 10,
                scan_truncated: false,
                pending_notifications: 3,
                acknowledged_notifications: 7,
                stranded_notifications: 2,
                oldest_pending_age_seconds: Some(41),
                epochs: vec![gents::BackgroundCompletionEpochDiagnostic {
                    root_request_id: "wake-1".to_string(),
                    active_request_id: "wake-1".to_string(),
                    session_id: "session-1".to_string(),
                    coalescing_key: "background_completion:parent".to_string(),
                    state: "exhausted".to_string(),
                    attempt_count: 4,
                    retry_count: 3,
                    max_retries: 3,
                    input_through_sequence: Some(12),
                    notification_count: 2,
                    acknowledged_notification_count: 0,
                    attempted_notification_keys: vec!["notification-1".to_string()],
                    acknowledged_notification_keys: Vec::new(),
                    pending_notification_keys: vec!["notification-1".to_string()],
                    pending_age_seconds: Some(41),
                    last_failure: Some("provider failed".to_string()),
                    next_retry_at: None,
                    created_at: None,
                    claimed_at: None,
                    terminalized_at: None,
                }],
            }),
        );
        let rendered = lines.join("\n");
        assert!(rendered.contains(
            r#"gents_background_completion_pending_notifications{node_did="did:agent:amy"} 3"#
        ));
        assert!(rendered.contains(
            r#"gents_background_completion_stranded_notifications{node_did="did:agent:amy"} 2"#
        ));
        assert!(rendered.contains(
            r#"gents_background_completion_epochs{node_did="did:agent:amy",state="exhausted"} 1"#
        ));
    }

    #[test]
    fn p2p_metrics_render_configured_admission_and_live_counts() {
        let mut lines = Vec::new();
        render_p2p_metrics(
            &mut lines,
            Some(&P2pMetricsSnapshot {
                enabled: true,
                connected_peers: 3,
                replicators: 2,
                admission: Some(crate::shared::P2pAdmissionState {
                    max_pending_dags: 1000,
                    max_concurrent_push_tasks: 8,
                    max_concurrent_dag_fetches: 4,
                    rate_limit_burst: 500,
                    rate_limit_rate: 50.0,
                }),
                sync_status: Some(gents::P2pSyncStatusSnapshot {
                    push_backlog: gents::P2pPushBacklogSnapshot {
                        queue_item_capacity: 128,
                        queue_byte_capacity: 1_048_576,
                        per_peer_active_cap: 2,
                        worker_count: 8,
                        queued_items: 7,
                        queued_bytes: 4_096,
                        active_jobs: 3,
                        enqueued_total: 101,
                        coalesced_total: 11,
                        rejected_items_total: 5,
                        rejected_bytes_total: 2,
                        completed_total: 79,
                        failed_total: 4,
                        stale_head_retirements_total: 17,
                        peer_capacity_parks_total: 13,
                        per_peer: vec![gents::P2pPeerBacklogSnapshot {
                            peer_id: "peer-a".to_string(),
                            queued_items: 4,
                            queued_bytes: 2_048,
                            active_jobs: 1,
                            consecutive_failures: 3,
                            cooldown_remaining_ms: 750,
                        }],
                        ..Default::default()
                    },
                    broadcast_coalesced_total: 41,
                    push_updates_coalesced_total: 43,
                    gossip_direction_filtered_total: 47,
                    pending_dags: 13,
                    pending_dag_capacity: 1_000,
                    persisted_pending_dags: 17,
                    persisted_pending_dag_capacity: 4_000,
                    pending_resync_in_flight: true,
                    retained_background_tasks: 6,
                    missing_link_retries: 23,
                    pending_dag_resolved: 29,
                    pending_dag_expired: 31,
                    single_flight_suppressed: 37,
                    already_merged_fast_path: 53,
                    pending_dag_capacity_shed: 59,
                    pending_dag_retry_dispatched: 61,
                    pending_dag_retry_suppressed: 67,
                    pending_dag_fetch_deferred_unavailable: 69,
                    pending_dag_fetch_deferred_contention: 68,
                    pending_dag_fetch_deferred_busy: 76,
                    next_pending_retry_in_ms: Some(71),
                    pending_dag_terminal_quarantined: 73,
                    quarantined_pending_dags: 79,
                    ..Default::default()
                }),
                stale: false,
                peer_id: Some("1dd587be0123456789".to_string()),
                transport: Some("iroh".to_string()),
            }),
        );
        let rendered = lines.join("\n");
        assert!(rendered.contains(
            r#"gents_p2p_self_info{peer_id="1dd587be0123456789",peer="1dd587be",transport="iroh"} 1"#
        ));
        assert_valid_exposition(&rendered, false);
        assert!(rendered.contains("gents_p2p_enabled 1"));
        assert!(rendered.contains("gents_p2p_status_stale 0"));
        assert!(rendered.contains("gents_p2p_connected_peers 3"));
        assert!(rendered.contains("gents_p2p_replicators 2"));
        assert!(rendered.contains("gents_p2p_admission_max_pending_dags 1000"));
        assert!(rendered.contains("gents_p2p_admission_max_concurrent_push_tasks 8"));
        assert!(rendered.contains("gents_p2p_admission_max_concurrent_dag_fetches 4"));
        assert!(rendered.contains("gents_p2p_admission_rate_limit_burst 500"));
        assert!(rendered.contains("gents_p2p_admission_rate_limit_rate 50"));
        assert!(rendered.contains("gents_p2p_sync_status_available 1"));
        assert!(rendered.contains("gents_p2p_push_queue_items 7"));
        assert!(rendered.contains("gents_p2p_push_queue_bytes 4096"));
        assert!(rendered.contains("gents_p2p_push_active_jobs 3"));
        assert!(rendered.contains("gents_p2p_pending_dags 13"));
        assert!(rendered.contains("gents_p2p_persisted_pending_dags 17"));
        assert!(rendered.contains("gents_p2p_quarantined_pending_dags 79"));
        assert!(rendered.contains("gents_p2p_push_rejected_items_total 5"));
        assert!(rendered.contains("gents_p2p_push_stale_head_retirements_total 17"));
        assert!(rendered.contains("gents_p2p_push_peer_capacity_parks_total 13"));
        assert!(rendered.contains("gents_p2p_broadcast_coalesced_total 41"));
        assert!(rendered.contains("gents_p2p_push_updates_coalesced_total 43"));
        assert!(rendered.contains("gents_p2p_missing_link_retries_total 23"));
        assert!(rendered.contains("gents_p2p_gossip_direction_filtered_total 47"));
        assert!(rendered.contains("gents_p2p_pending_dag_capacity_shed_total 59"));
        assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_unavailable_total 69"));
        assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_contention_total 68"));
        assert!(rendered.contains("gents_p2p_pending_dag_fetch_deferred_busy_total 76"));
        assert!(rendered.contains("gents_p2p_single_flight_suppressed_total 37"));
        assert!(rendered.contains("gents_p2p_already_merged_fast_path_total 53"));
        assert!(rendered.contains("gents_p2p_pending_dag_terminal_quarantined_total 73"));
        assert!(rendered.contains(r#"gents_p2p_peer_consecutive_failures{peer_id="peer-a"} 3"#));
        assert!(rendered
            .contains(r#"gents_p2p_peer_cooldown_remaining_milliseconds{peer_id="peer-a"} 750"#));
    }

    /// Checks the text exposition format: comments are HELP or TYPE, every
    /// sample's family is typed exactly once before it, values are numbers,
    /// labels are well formed and no series repeats. `grouped` additionally
    /// requires each family's samples to be contiguous after its TYPE line.
    pub(super) fn assert_valid_exposition(text: &str, grouped: bool) {
        let mut typed = std::collections::BTreeMap::<String, String>::new();
        let mut series = std::collections::BTreeSet::<String>::new();
        let mut closed = std::collections::BTreeSet::<String>::new();
        let mut current: Option<String> = None;
        let valid_name = |name: &str| {
            !name.is_empty()
                && !name.starts_with(|c: char| c.is_ascii_digit())
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':')
        };
        for line in text.lines().filter(|line| !line.is_empty()) {
            if let Some(comment) = line.strip_prefix("# ") {
                let mut parts = comment.splitn(3, ' ');
                let (kind, name) = (parts.next().unwrap(), parts.next().unwrap_or_default());
                assert!(valid_name(name), "bad family name: {line}");
                match kind {
                    "HELP" => {}
                    "TYPE" => {
                        let metric_type = parts.next().unwrap_or_default();
                        assert!(
                            ["gauge", "counter", "histogram", "summary", "untyped"]
                                .contains(&metric_type),
                            "bad type: {line}"
                        );
                        assert!(
                            typed
                                .insert(name.to_string(), metric_type.to_string())
                                .is_none(),
                            "family typed twice: {name}"
                        );
                        if grouped {
                            if let Some(previous) = current.replace(name.to_string()) {
                                closed.insert(previous);
                            }
                        }
                    }
                    _ => panic!("unexpected comment: {line}"),
                }
                continue;
            }
            let (series_key, value) = line.rsplit_once(' ').expect("sample has a value");
            assert!(value.parse::<f64>().is_ok(), "non-numeric value: {line}");
            let name = series_key.split('{').next().unwrap();
            let metric_type = typed
                .get(name)
                .unwrap_or_else(|| panic!("sample before TYPE: {line}"));
            if metric_type == "counter" {
                assert!(name.ends_with("_total"), "counter without _total: {name}");
            }
            if grouped {
                assert_eq!(
                    current.as_deref(),
                    Some(name),
                    "family not contiguous: {line}"
                );
                assert!(!closed.contains(name), "family reopened: {line}");
            }
            if let Some(labels) = series_key.strip_prefix(name) {
                if !labels.is_empty() {
                    let inner = labels
                        .strip_prefix('{')
                        .and_then(|rest| rest.strip_suffix('}'))
                        .unwrap_or_else(|| panic!("bad label set: {line}"));
                    let mut keys = std::collections::BTreeSet::new();
                    for pair in inner.split("\",").filter(|pair| !pair.is_empty()) {
                        let (key, value) = pair
                            .split_once("=\"")
                            .unwrap_or_else(|| panic!("bad label: {line}"));
                        assert!(valid_name(key), "bad label name: {line}");
                        assert!(keys.insert(key), "duplicate label {key}: {line}");
                        assert!(!value.trim_end_matches('"').contains('\n'));
                    }
                }
            }
            assert!(
                series.insert(series_key.to_string()),
                "duplicate series: {line}"
            );
        }
    }

    fn rich_sync_status() -> gents::P2pSyncStatusSnapshot {
        gents::P2pSyncStatusSnapshot {
            request_dispatch: gents::P2pRequestDispatchSnapshot {
                request_capacity: 32,
                active_requests: 5,
                active_requests_high_water: 30,
                recovery_capacity: 8,
                active_recovery: 7,
                active_recovery_high_water: 8,
                rejection_capacity: 4,
                active_rejections: 1,
                active_rejections_high_water: 2,
                completion_capacity: 16,
                active_completions: 3,
                active_completions_high_water: 9,
                saturated_total: 1_611_200,
                admission_saturated_total: 29,
                recovery_saturated_total: 1_611_171,
                rejection_dropped_total: 12,
            },
            push_backlog: gents::P2pPushBacklogSnapshot {
                enqueued_total: 512_729,
                failed_total: 82_089,
                head_hints_enqueued_document: 10,
                head_hints_enqueued_collection: 11,
                head_hints_sent_document: 12,
                head_hints_sent_collection: 13,
                head_hints_acked_document: 14,
                head_hints_acked_collection: 15,
                head_hints_nacked_capacity: 16,
                head_hints_nacked_other: 17,
                head_hints_failed_transport: 47_037,
                head_hints_failed_local: 18,
                ..Default::default()
            },
            push_retry_markers: gents::P2pPushRetryMarkerSnapshot {
                document_markers: 3,
                collection_markers: 4,
                scheduled_peers: 2,
                oldest_scheduled_retry_unix: Some(1_800_000_000),
            },
            pending_dag_high_water: 900,
            persisted_pending_dag_high_water: 950,
            inbound_budget: vec![gents::P2pInboundClassSnapshot {
                class: "sync".to_string(),
                capacity_bytes: 1_048_576,
                held_bytes: 4_096,
                held_bytes_high_water: 65_536,
                busy_resets: 5,
                oversized_resets: 6,
            }],
            peer_fetches: vec![
                gents::P2pPeerFetchSnapshot {
                    peer: "1dd587be-hub-peer".to_string(),
                    fetches: 82_928,
                    bytes: 13_600_000_000,
                    delivery_rate_bytes_per_sec: 2_048,
                    min_rtt_micros: Some(400),
                    success_ppm: 20_897,
                    timeout_ppm: 970_000,
                    partial_ppm: 9_103,
                    busy_ppm: 1_200,
                    in_flight: 2,
                    in_flight_bytes: 1_024,
                    success_latency_micros: Some(84_000),
                    useful_ewma_ppm: Some(170_000),
                    attempt_cost_micros: Some(490_000),
                    expected_useful_micros: Some(2_882_352),
                    ewma_age_millis: Some(1_500),
                },
                gents::P2pPeerFetchSnapshot {
                    peer: "abcdef0123-spoke".to_string(),
                    fetches: 10,
                    success_ppm: 1_000_000,
                    min_rtt_micros: None,
                    ..Default::default()
                },
            ],
            non_authoritative_broadcast_tasks: 3,
            non_authoritative_broadcast_high_water: 7,
            non_authoritative_broadcast_rejected_total: 9,
            car_requested_cids: 1_400_000_000,
            car_present_cids: 1_350_000_000,
            car_served_cids: 1_340_000_000,
            car_filtered_cids: 10,
            provider_rotations: 36_455,
            provider_busy_rotations: 412,
            provider_demotions_total: 37,
            provider_cooldown_skips_total: 205,
            car_busy_responses: 96,
            pending_dag_registered: 11_329,
            pending_dag_retry_dispatched: 20_852,
            pending_dag_retry_suppressed: 44,
            pending_dag_fetch_exhausted: 9_871,
            pending_dag_terminal_merged: 10_523,
            next_pending_retry_in_ms: Some(1_500),
            pending_dag_attempt_ewma_median_ms: Some(4_500),
            pending_dag_attempt_ewma_max_ms: Some(35_000),
            pending_dag_no_progress_median_ppm: Some(500_007),
            pending_dag_tie_reorders: 12,
            pending_dag_peer_occupancy: vec![gents::P2pPendingPeerOccupancySnapshot {
                peer: "1dd587be-hub-peer".to_string(),
                held: 3,
                occupancy_q8: 640,
            }],
            peers: vec![
                gents::P2pPeerSummarySnapshot {
                    peer_id: "1dd587be-hub-peer".to_string(),
                    connected: true,
                    last_seen_at: "2026-11-02T15:14:50Z".to_string(),
                    last_block_received_at: Some("2026-11-02T15:14:00Z".to_string()),
                    last_head_acked_at: Some("2026-11-02T15:10:00Z".to_string()),
                },
                gents::P2pPeerSummarySnapshot {
                    peer_id: "abcdef0123-spoke".to_string(),
                    connected: false,
                    last_seen_at: "not a timestamp".to_string(),
                    last_block_received_at: None,
                    last_head_acked_at: None,
                },
            ],
            ..Default::default()
        }
    }

    fn scrape_now() -> DateTime<Utc> {
        rfc3339_timestamp("2026-11-02T15:15:00Z").unwrap()
    }

    #[test]
    fn sync_diagnostics_render_every_decoded_field_with_bounded_labels() {
        let mut lines = Vec::new();
        p2p_sync::render_sync_diagnostics(&mut lines, Some(&rich_sync_status()), scrape_now());
        let rendered = lines.join("\n");
        assert_valid_exposition(&rendered, true);
        let hub = r#"peer_id="1dd587be-hub-peer",peer="1dd587be""#;
        let spoke = r#"peer_id="abcdef0123-spoke",peer="abcdef01""#;
        for sample in [
            r#"gents_p2p_request_dispatch_capacity{lane="recovery"} 8"#.to_string(),
            r#"gents_p2p_request_dispatch_active{lane="recovery"} 7"#.to_string(),
            r#"gents_p2p_request_dispatch_high_water{lane="recovery"} 8"#.to_string(),
            r#"gents_p2p_request_dispatch_capacity{lane="request"} 32"#.to_string(),
            r#"gents_p2p_request_dispatch_active{lane="rejection"} 1"#.to_string(),
            r#"gents_p2p_request_dispatch_high_water{lane="completion"} 9"#.to_string(),
            r#"gents_p2p_request_dispatch_saturated_total{lane="any"} 1611200"#.to_string(),
            r#"gents_p2p_request_dispatch_saturated_total{lane="admission"} 29"#.to_string(),
            r#"gents_p2p_request_dispatch_saturated_total{lane="recovery"} 1611171"#.to_string(),
            "gents_p2p_request_dispatch_rejection_dropped_total 12".to_string(),
            "gents_p2p_pending_dag_registered_total 11329".to_string(),
            "gents_p2p_pending_dag_fetch_exhausted_total 9871".to_string(),
            "gents_p2p_pending_dag_terminal_merged_total 10523".to_string(),
            "gents_p2p_pending_dag_retry_dispatched_total 20852".to_string(),
            "gents_p2p_pending_dag_retry_suppressed_total 44".to_string(),
            "gents_p2p_provider_rotations_total 36455".to_string(),
            "gents_p2p_provider_busy_rotations_total 412".to_string(),
            "gents_p2p_provider_demotions_total 37".to_string(),
            "gents_p2p_provider_cooldown_skips_total 205".to_string(),
            "gents_p2p_car_busy_responses_total 96".to_string(),
            "gents_p2p_pending_dag_high_water 900".to_string(),
            "gents_p2p_persisted_pending_dag_high_water 950".to_string(),
            "gents_p2p_next_pending_retry_seconds 1.5".to_string(),
            r#"gents_p2p_car_cids_total{outcome="present"} 1350000000"#.to_string(),
            r#"gents_p2p_car_cids_total{outcome="served"} 1340000000"#.to_string(),
            r#"gents_p2p_head_hints_total{outcome="acked",scope="collection"} 15"#.to_string(),
            r#"gents_p2p_head_hints_total{outcome="failed_transport",scope="all"} 47037"#
                .to_string(),
            r#"gents_p2p_inbound_budget_held_bytes{class="sync"} 4096"#.to_string(),
            r#"gents_p2p_inbound_budget_capacity_bytes{class="sync"} 1048576"#.to_string(),
            r#"gents_p2p_inbound_budget_held_high_water_bytes{class="sync"} 65536"#.to_string(),
            r#"gents_p2p_inbound_budget_resets_total{class="sync",reason="oversized"} 6"#
                .to_string(),
            "gents_p2p_non_authoritative_broadcast_tasks 3".to_string(),
            "gents_p2p_non_authoritative_broadcast_rejected_total 9".to_string(),
            r#"gents_p2p_push_retry_markers{scope="collection"} 4"#.to_string(),
            "gents_p2p_push_retry_scheduled_peers 2".to_string(),
            "gents_p2p_push_retry_oldest_scheduled_timestamp_seconds 1800000000".to_string(),
            format!("gents_p2p_peer_fetch_total{{{hub}}} 82928"),
            format!("gents_p2p_peer_fetch_bytes_total{{{hub}}} 13600000000"),
            format!("gents_p2p_peer_fetch_success_ppm{{{hub}}} 20897"),
            format!("gents_p2p_peer_fetch_timeout_ppm{{{hub}}} 970000"),
            format!("gents_p2p_peer_fetch_partial_ppm{{{hub}}} 9103"),
            format!("gents_p2p_peer_fetch_busy_ppm{{{hub}}} 1200"),
            format!("gents_p2p_peer_fetch_min_rtt_seconds{{{hub}}} 0.0004"),
            format!("gents_p2p_peer_fetch_delivery_rate_bytes_per_second{{{hub}}} 2048"),
            format!("gents_p2p_peer_fetch_in_flight{{{hub}}} 2"),
            format!("gents_p2p_peer_fetch_in_flight_bytes{{{hub}}} 1024"),
            format!("gents_p2p_peer_fetch_success_ppm{{{spoke}}} 1000000"),
            format!("gents_p2p_peer_fetch_success_latency_seconds{{{hub}}} 0.084"),
            format!("gents_p2p_peer_fetch_useful_ewma_ppm{{{hub}}} 170000"),
            format!("gents_p2p_peer_fetch_attempt_cost_seconds{{{hub}}} 0.49"),
            format!("gents_p2p_peer_fetch_expected_useful_seconds{{{hub}}} 2.882352"),
            format!("gents_p2p_peer_fetch_ewma_age_seconds{{{hub}}} 1.5"),
            "gents_p2p_pending_dag_tie_reorders_total 12".to_string(),
            "gents_p2p_pending_dag_attempt_ewma_median_seconds 4.5".to_string(),
            "gents_p2p_pending_dag_attempt_ewma_max_seconds 35".to_string(),
            "gents_p2p_pending_dag_no_progress_median_ppm 500007".to_string(),
            format!("gents_p2p_pending_dag_peer_held{{{hub}}} 3"),
            format!("gents_p2p_pending_dag_peer_occupancy{{{hub}}} 2.5"),
            r#"gents_p2p_peer_series_dropped{family="pending_dag"} 0"#.to_string(),
            format!("gents_p2p_peer_connected{{{hub}}} 1"),
            format!("gents_p2p_peer_connected{{{spoke}}} 0"),
            format!("gents_p2p_peer_last_seen_age_seconds{{{hub}}} 10"),
            format!("gents_p2p_peer_last_block_received_age_seconds{{{hub}}} 60"),
            format!("gents_p2p_peer_last_head_acked_age_seconds{{{hub}}} 300"),
            r#"gents_p2p_peer_series_dropped{family="fetch"} 0"#.to_string(),
            r#"gents_p2p_peer_series_dropped{family="peer"} 0"#.to_string(),
        ] {
            assert!(
                rendered.lines().any(|line| line == sample),
                "missing {sample}"
            );
        }
        for absent in [
            format!("gents_p2p_peer_fetch_min_rtt_seconds{{{spoke}}}"),
            format!("gents_p2p_peer_fetch_success_latency_seconds{{{spoke}}}"),
            format!("gents_p2p_peer_fetch_expected_useful_seconds{{{spoke}}}"),
            format!("gents_p2p_peer_last_seen_age_seconds{{{spoke}}}"),
            format!("gents_p2p_peer_last_head_acked_age_seconds{{{spoke}}}"),
        ] {
            assert!(
                !rendered.contains(&absent),
                "unmeasured {absent} must be absent"
            );
        }
    }

    #[test]
    fn unavailable_sync_status_declares_families_without_zero_samples() {
        let mut lines = Vec::new();
        p2p_sync::render_sync_diagnostics(&mut lines, None, scrape_now());
        let rendered = lines.join("\n");
        assert_valid_exposition(&rendered, true);
        assert!(rendered.contains("# TYPE gents_p2p_request_dispatch_saturated_total counter"));
        assert!(rendered.lines().all(|line| line.starts_with('#')));
    }

    #[test]
    fn per_peer_series_are_capped_and_the_overflow_is_counted() {
        let status = gents::P2pSyncStatusSnapshot {
            peer_fetches: (0..300)
                .map(|index| gents::P2pPeerFetchSnapshot {
                    peer: format!("peer-{index:04}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let mut lines = Vec::new();
        p2p_sync::render_sync_diagnostics(&mut lines, Some(&status), scrape_now());
        let rendered = lines.join("\n");
        assert_valid_exposition(&rendered, true);
        assert_eq!(
            rendered
                .lines()
                .filter(|line| line.starts_with("gents_p2p_peer_fetch_total{"))
                .count(),
            256
        );
        assert!(rendered
            .contains(r#"gents_p2p_peer_fetch_total{peer_id="peer-0255",peer="peer-025"} 0"#));
        assert!(!rendered.contains(r#"peer_id="peer-0256""#));
        assert!(rendered
            .lines()
            .any(|line| line == r#"gents_p2p_peer_series_dropped{family="fetch"} 44"#));
    }

    #[test]
    fn short_peer_id_respects_char_boundaries() {
        assert_eq!(p2p_sync::short_peer_id("1dd587be0123"), "1dd587be");
        assert_eq!(p2p_sync::short_peer_id("abc"), "abc");
        assert_eq!(p2p_sync::short_peer_id("ééééééééé"), "éééééééé");
    }

    #[test]
    fn metrics_query_envelope_treats_null_probe_status_as_unknown() {
        let envelope: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
            "NodeRuntime": [],
            "NodeReadiness": [],
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
    fn nullable_recorded_model_keeps_inference_metrics_available() {
        for model in [
            serde_json::json!({}),
            serde_json::json!({"model_name": null}),
        ] {
            let mut call = serde_json::json!({"call_state":"completed", "call_kind":"inference"});
            call.as_object_mut()
                .unwrap()
                .extend(model.as_object().unwrap().clone());
            let page: InferenceMetricsPageData =
                serde_json::from_value(serde_json::json!({"InferenceCall":[call]})).unwrap();
            let families = build_inference_metric_families(&InferenceMetricsQueryData {
                nodes: vec![],
                agents: vec![],
                calls: page.calls,
                window_seconds: INFERENCE_METRICS_WINDOW_SECS,
            });
            assert_eq!(families.request_totals.len(), 1);
            assert_eq!(
                families.request_totals.keys().next().unwrap().model,
                "unknown"
            );
        }
    }

    #[test]
    fn metric_labels_escape_backslash_quote_and_newline() {
        let mut lines = Vec::new();
        push_metric_sample(
            &mut lines,
            "gents_runtime_active_generation",
            &[("node_did", "did:key:zA\\b\"c\nd".to_string())],
            1,
        );
        assert_eq!(
            lines[0], r#"gents_runtime_active_generation{node_did="did:key:zA\\b\"c\nd"} 1"#,
            "backslash must be doubled and quote/newline escaped, in that order"
        );
    }

    #[test]
    fn inference_metrics_group_by_agent_backend_model_and_status() {
        let data = InferenceMetricsQueryData {
            nodes: vec![InferenceNodeRow {
                node_did: "did:key:zAgent".to_string(),
                display_name: "observability-steward".to_string(),
            }],
            agents: vec![InferenceAgentRow {
                agent_id: "agent_config-1".to_string(),
                node_did: "did:key:zAgent".to_string(),
                display_name: "fallback-agent_config-name".to_string(),
            }],
            calls: vec![
                InferenceCallMetricRow {
                    node_did: "did:key:zAgent".to_string(),
                    agent_id: "agent_config-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "completed".to_string(),
                    ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
                    model_name: "fixture-model".to_string(),
                    call_kind: "inference".to_string(),
                    usage: TokenUsageColumns {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(4),
                        ..Default::default()
                    },
                },
                InferenceCallMetricRow {
                    node_did: "did:key:zAgent".to_string(),
                    agent_id: "agent_config-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "completed".to_string(),
                    ended_at: "2026-06-13T10:00:01.000000000+00:00".to_string(),
                    model_name: "fixture-model".to_string(),
                    call_kind: "inference".to_string(),
                    usage: TokenUsageColumns {
                        prompt_tokens: Some(7),
                        completion_tokens: Some(3),
                        ..Default::default()
                    },
                },
                InferenceCallMetricRow {
                    node_did: "did:key:zAgent".to_string(),
                    agent_id: "agent_config-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "failed".to_string(),
                    ended_at: "2026-06-13T10:00:02.000000000+00:00".to_string(),
                    model_name: "fixture-model".to_string(),
                    call_kind: "inference".to_string(),
                    usage: TokenUsageColumns {
                        prompt_tokens: Some(5),
                        completion_tokens: None,
                        ..Default::default()
                    },
                },
                InferenceCallMetricRow {
                    node_did: "did:key:zAgent".to_string(),
                    agent_id: "agent_config-1".to_string(),
                    backend_id: "backend-1".to_string(),
                    call_state: "running".to_string(),
                    ended_at: "2026-06-13T10:00:03.000000000+00:00".to_string(),
                    model_name: "fixture-model".to_string(),
                    call_kind: "inference".to_string(),
                    usage: TokenUsageColumns {
                        prompt_tokens: Some(99),
                        completion_tokens: Some(99),
                        ..Default::default()
                    },
                },
            ],
            window_seconds: INFERENCE_METRICS_WINDOW_SECS,
        };

        let families = build_inference_metric_families(&data);
        let completed_key = InferenceRequestMetricKey {
            agent: "observability-steward".to_string(),
            node_did: "did:key:zAgent".to_string(),
            backend_id: "backend-1".to_string(),
            model: "fixture-model".to_string(),
            status: "completed".to_string(),
            call_kind: "inference".to_string(),
        };
        let failed_key = InferenceRequestMetricKey {
            status: "failed".to_string(),
            ..completed_key.clone()
        };
        let token_key = InferenceTokenMetricKey {
            agent: "observability-steward".to_string(),
            node_did: "did:key:zAgent".to_string(),
            backend_id: "backend-1".to_string(),
            model: "fixture-model".to_string(),
            call_kind: "inference".to_string(),
        };

        assert_eq!(families.request_totals.get(&completed_key), Some(&2));
        assert_eq!(families.request_totals.get(&failed_key), Some(&1));
        let summary = families.usage[&token_key].unwrap();
        assert_eq!(summary.input.known, 22);
        assert_eq!(summary.output.known, 7);
        assert_eq!(summary.incomplete_calls, 3);
    }

    #[test]
    fn inference_metrics_query_reads_only_the_scrape_window() {
        let start = DateTime::parse_from_rfc3339("2026-10-22T21:15:00.500+00:00")
            .unwrap()
            .with_timezone(&Utc);
        let bound = inference_metrics_ended_lower_bound(&start);
        assert_eq!(bound, "2026-10-22T21:15:00");
        let inside = (start + Duration::milliseconds(1)).to_rfc3339();
        assert!(bound.as_str() <= inside.as_str());
        assert!(bound.as_str() <= "2026-10-22T21:15:00+00:00");
        let query = inference_metrics_query(500, 0, false, &start);
        assert!(query.contains(r#"ended_at: { _ge: "2026-10-22T21:15:00" }"#));
    }

    #[test]
    fn render_inference_metrics_emits_windowed_gauge_families() {
        let data = InferenceMetricsQueryData {
            nodes: vec![],
            agents: vec![InferenceAgentRow {
                agent_id: "agent-1".to_string(),
                node_did: "did:key:zAgent".to_string(),
                display_name: "agent \"friendly\"".to_string(),
            }],
            calls: vec![InferenceCallMetricRow {
                node_did: "did:key:zAgent".to_string(),
                agent_id: "agent-1".to_string(),
                backend_id: "backend-1".to_string(),
                call_state: "completed".to_string(),
                ended_at: "2026-06-13T10:00:00.000000000+00:00".to_string(),
                model_name: "model\none".to_string(),
                call_kind: "inference".to_string(),
                usage: TokenUsageColumns {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(3),
                    ..Default::default()
                },
            }],
            window_seconds: INFERENCE_METRICS_WINDOW_SECS,
        };

        let mut lines = Vec::new();
        render_inference_metrics(&mut lines, &data);
        let body = lines.join("\n");

        assert!(body.contains("# TYPE gents_inference_metrics_window_seconds gauge"));
        assert!(body.contains("gents_inference_metrics_window_seconds 300"));
        assert!(body.contains("# TYPE gents_inference_requests_window_count gauge"));
        assert!(body.contains(
            "gents_inference_requests_window_count{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",status=\"completed\",call_kind=\"inference\"} 1"
        ));
        assert!(body.contains(
            "gents_inference_prompt_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",call_kind=\"inference\"} 10"
        ));
        assert!(body.contains(
            "gents_inference_completion_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",node_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",call_kind=\"inference\"} 3"
        ));
    }
}
