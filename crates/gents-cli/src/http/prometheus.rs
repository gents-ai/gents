use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use gents_protocol::row::{
    decode_behavior_readiness_snapshot, project_behavior_readiness_summary,
    AgentBehaviorReadinessRow, ProjectedBehaviorReadinessSummary,
};
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
    pub(crate) behavior_readiness: Vec<AgentBehaviorReadinessRow>,
    pub(crate) inference_backends: Vec<MetricsBackendRow>,
    pub(crate) liveness: RuntimeLivenessSnapshot,
}

#[derive(Debug, Deserialize)]
struct MetricsQueryEnvelope {
    #[serde(rename = "AgentRuntime", default)]
    agent_runtimes: Vec<MetricsRuntimeRow>,
    #[serde(rename = "AgentBehaviorReadiness", default)]
    behavior_readiness: Vec<AgentBehaviorReadinessRow>,
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

#[derive(Debug, Clone, Default)]
pub(crate) struct P2pMetricsSnapshot {
    pub(crate) enabled: bool,
    pub(crate) connected_peers: usize,
    pub(crate) replicators: usize,
    pub(crate) admission: Option<crate::shared::P2pAdmissionState>,
    pub(crate) sync_status: Option<gents::P2pSyncStatusSnapshot>,
    pub(crate) stale: bool,
}

pub(crate) async fn render_prometheus_metrics(
    graphql: &str,
    local_agent_did: &str,
    measured_backend_health: &HashMap<String, gents::BackendHealthSnapshot>,
    p2p: Option<&P2pMetricsSnapshot>,
) -> Result<String> {
    let data = load_metrics_query_data(graphql, local_agent_did).await?;
    let data = with_local_native_executors(data);
    let inference_metrics = load_inference_metrics_query_data(graphql).await?;
    let background_completion = gents::load_background_completion_diagnostics(
        &gents::config_client::ConfigAccess::Graphql(graphql.to_string()),
        local_agent_did,
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
        "One-hot process lifecycle state for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_reconcile_phase",
        "One-hot reconcile phase for each agent runtime.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_last_reconcile_result",
        "One-hot last reconcile result for each agent runtime.",
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
        "gents_runtime_runnable_behaviors",
        "Number of ready behaviors in the authoritative behavior-readiness snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_unavailable_behaviors",
        "Number of unavailable behaviors in the authoritative behavior-readiness snapshot.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_behavior_readiness_observed",
        "Whether the authoritative behavior-readiness snapshot is valid and current.",
    );
    push_metric_prelude(
        &mut lines,
        "gents_runtime_last_reconcile_completed_at_seconds",
        "Unix timestamp of the last completed reconcile.",
    );

    for readiness in runtime_inventory(&data) {
        let agent_did = readiness.agent_did.clone();
        let runtime = data
            .agent_runtimes
            .iter()
            .find(|runtime| runtime.agent_did == agent_did);
        let lifecycle = decode_behavior_readiness_snapshot(readiness, &agent_did).ok();
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
                &[
                    ("agent_did", agent_did.clone()),
                    ("state", state.to_string()),
                ],
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
                    "gents_runtime_last_reconcile_result",
                    &[
                        ("agent_did", agent_did.clone()),
                        ("result", result.to_string()),
                    ],
                    i64::from(runtime.last_reconcile_result == result),
                );
            }
        }
        push_metric_sample(
            &mut lines,
            "gents_runtime_active_generation",
            &[("agent_did", agent_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.active_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_metric_sample(
            &mut lines,
            "gents_runtime_router_generation",
            &[("agent_did", agent_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.router_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_behavior_readiness_metrics(&mut lines, &agent_did, &data.behavior_readiness);
        if let Some(timestamp) = runtime
            .and_then(|runtime| rfc3339_timestamp_seconds(&runtime.last_reconcile_completed_at))
        {
            push_metric_sample(
                &mut lines,
                "gents_runtime_last_reconcile_completed_at_seconds",
                &[("agent_did", agent_did)],
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
        "Number of processing AgentRequest rows ignored because they belong to a different agent DID.",
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
        "Number of running AgentToolCall rows ignored because they belong to a different agent DID.",
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
    render_background_completion_metrics(
        &mut lines,
        local_agent_did,
        background_completion.as_ref(),
    );
    render_p2p_metrics(&mut lines, p2p);

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn runtime_inventory(data: &MetricsQueryData) -> impl Iterator<Item = &AgentBehaviorReadinessRow> {
    data.behavior_readiness.iter()
}

fn push_behavior_readiness_metrics(
    lines: &mut Vec<String>,
    agent_did: &str,
    rows: &[AgentBehaviorReadinessRow],
) {
    push_behavior_readiness_metrics_at(lines, agent_did, rows, chrono::Utc::now());
}

fn push_behavior_readiness_metrics_at(
    lines: &mut Vec<String>,
    agent_did: &str,
    rows: &[AgentBehaviorReadinessRow],
    observed_at: chrono::DateTime<chrono::Utc>,
) {
    let readiness = rows.iter().find(|row| row.agent_did == agent_did);
    let (ready, unavailable, observed) =
        match project_behavior_readiness_summary(readiness, agent_did, observed_at) {
            ProjectedBehaviorReadinessSummary::Observed(summary) => (
                i64::try_from(summary.ready_count).unwrap_or(i64::MAX),
                i64::try_from(summary.unavailable_behaviors.len()).unwrap_or(i64::MAX),
                1,
            ),
            ProjectedBehaviorReadinessSummary::Unknown(_) => (0, 0, 0),
        };
    let labels = [("agent_did", agent_did.to_string())];
    push_metric_sample(lines, "gents_runtime_runnable_behaviors", &labels, ready);
    push_metric_sample(
        lines,
        "gents_runtime_unavailable_behaviors",
        &labels,
        unavailable,
    );
    push_metric_sample(
        lines,
        "gents_runtime_behavior_readiness_observed",
        &labels,
        observed,
    );
}

fn render_background_completion_metrics(
    lines: &mut Vec<String>,
    agent_did: &str,
    diagnostics: Option<&gents::BackgroundCompletionDiagnostics>,
) {
    let labels = [("agent_did", agent_did.to_string())];
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
                ("agent_did", agent_did.to_string()),
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

    render_p2p_sync_metrics(lines, snapshot.sync_status.as_ref());

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
        "gents_inference_prompt_tokens_window_sum",
        "Prompt tokens reported by terminal inference calls ended inside the trailing scrape window; this gauge is not cumulative.",
    );
    for (key, total) in families.prompt_token_totals {
        push_metric_sample(
            lines,
            "gents_inference_prompt_tokens_window_sum",
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
        "gents_inference_completion_tokens_window_sum",
        "Completion tokens reported by terminal inference calls ended inside the trailing scrape window; this gauge is not cumulative.",
    );
    for (key, total) in families.completion_token_totals {
        push_metric_sample(
            lines,
            "gents_inference_completion_tokens_window_sum",
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
                reconcile_phase
                last_reconcile_result
                last_reconcile_completed_at
            }
            AgentBehaviorReadiness {
                agent_did
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
        behavior_readiness: envelope.behavior_readiness,
        inference_backends: envelope.inference_backends,
        liveness,
    })
}

pub(crate) fn with_local_native_executors(mut data: MetricsQueryData) -> MetricsQueryData {
    data.liveness = with_active_native_executors(data.liveness, gents::active_native_executors());
    data
}

pub(crate) fn push_backend_metrics(
    lines: &mut Vec<String>,
    backends: &[MetricsBackendRow],
    measured: &HashMap<String, gents::BackendHealthSnapshot>,
) {
    for backend in backends {
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
            backend.max_concurrent,
        );
        push_metric_sample(
            lines,
            "gents_backend_max_queue_depth",
            &[("backend_id", backend.backend_id.clone())],
            backend.max_queue_depth,
        );
        let measured_entry = measured.get(&backend.backend_id);
        let status = measured_entry
            .map(|entry| entry.state.as_str().to_string())
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
                "gents_backend_last_probe_seconds",
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
    use gents_protocol::row::{
        BehaviorReadinessEntry, BehaviorReadinessProcessState, BehaviorReadinessSnapshot,
        BehaviorReadinessState, BehaviorReadinessUnavailableReason,
        BEHAVIOR_READINESS_FORMAT_VERSION,
    };

    fn metrics_runtime(agent_did: &str) -> MetricsRuntimeRow {
        MetricsRuntimeRow {
            agent_did: agent_did.to_string(),
            reconcile_phase: "idle".to_string(),
            last_reconcile_result: "applied".to_string(),
            last_reconcile_completed_at: "2026-08-29T00:00:00Z".to_string(),
        }
    }

    fn metrics_readiness(agent_did: &str) -> AgentBehaviorReadinessRow {
        AgentBehaviorReadinessRow {
            agent_did: agent_did.to_string(),
            snapshot_json: serde_json::to_string(&BehaviorReadinessSnapshot {
                format_version: BEHAVIOR_READINESS_FORMAT_VERSION,
                process_state: BehaviorReadinessProcessState::Ready,
                active_generation: 4,
                router_generation: 4,
                default_behavior_id: "default".to_string(),
                behaviors: vec![
                    BehaviorReadinessEntry {
                        behavior_id: "default".to_string(),
                        state: BehaviorReadinessState::Ready,
                        reason: None,
                    },
                    BehaviorReadinessEntry {
                        behavior_id: "offline".to_string(),
                        state: BehaviorReadinessState::Unavailable,
                        reason: Some(
                            BehaviorReadinessUnavailableReason::BackendTemporarilyUnavailable,
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
    fn behavior_readiness_metrics_distinguish_observed_from_unknown() {
        let runtime = metrics_runtime("did:test:metrics");
        let mut lines = Vec::new();
        push_behavior_readiness_metrics_at(
            &mut lines,
            &runtime.agent_did,
            &[metrics_readiness("did:test:metrics")],
            readiness_observed_at(),
        );
        let rendered = lines.join("\n");
        assert!(rendered
            .contains(r#"gents_runtime_runnable_behaviors{agent_did="did:test:metrics"} 1"#));
        assert!(rendered
            .contains(r#"gents_runtime_unavailable_behaviors{agent_did="did:test:metrics"} 1"#));
        assert!(rendered.contains(
            r#"gents_runtime_behavior_readiness_observed{agent_did="did:test:metrics"} 1"#
        ));

        for rows in [
            Vec::new(),
            vec![AgentBehaviorReadinessRow {
                snapshot_json: "{}".to_string(),
                ..metrics_readiness("did:test:metrics")
            }],
            vec![AgentBehaviorReadinessRow {
                updated_at: "2026-08-28T23:59:00Z".to_string(),
                ..metrics_readiness("did:test:metrics")
            }],
        ] {
            let mut lines = Vec::new();
            push_behavior_readiness_metrics_at(
                &mut lines,
                &runtime.agent_did,
                &rows,
                readiness_observed_at(),
            );
            let rendered = lines.join("\n");
            assert!(rendered
                .contains(r#"gents_runtime_runnable_behaviors{agent_did="did:test:metrics"} 0"#));
            assert!(rendered.contains(
                r#"gents_runtime_unavailable_behaviors{agent_did="did:test:metrics"} 0"#
            ));
            assert!(rendered.contains(
                r#"gents_runtime_behavior_readiness_observed{agent_did="did:test:metrics"} 0"#
            ));
        }
    }

    #[test]
    fn metrics_runtime_inventory_is_readiness_not_diagnostics() {
        let readiness = metrics_readiness("did:test:readiness-only");
        let data = MetricsQueryData {
            agent_runtimes: Vec::new(),
            behavior_readiness: vec![readiness],
            inference_backends: Vec::new(),
            liveness: RuntimeLivenessSnapshot::default(),
        };
        assert_eq!(
            runtime_inventory(&data)
                .map(|row| row.agent_did.as_str())
                .collect::<Vec<_>>(),
            vec!["did:test:readiness-only"]
        );

        let diagnostics_only = MetricsQueryData {
            agent_runtimes: vec![metrics_runtime("did:test:diagnostics-only")],
            behavior_readiness: Vec::new(),
            inference_backends: Vec::new(),
            liveness: RuntimeLivenessSnapshot::default(),
        };
        assert_eq!(runtime_inventory(&diagnostics_only).count(), 0);
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
                max_concurrent: 4,
                max_queue_depth: 100,
                probe_status: probe_status.to_string(),
                last_probe: last_probe.map(str::to_string),
            }
        }
        fn measured_entry(
            backend_id: &str,
            state: gents::BackendHealthState,
            failure_count: u32,
            last_probe_at: &str,
        ) -> gents::BackendHealthSnapshot {
            gents::BackendHealthSnapshot {
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
            doc_row("sparks-cluster", "healthy", None),
            doc_row("workstation-1", "healthy", Some("2026-07-01T00:00:00Z")),
            doc_row("spark-2", "healthy", None),
            doc_row("codex", "healthy", Some("2026-07-06T00:00:00Z")),
            doc_row("unprobed-unknown", "unknown", None),
        ];
        let measured = HashMap::from([
            (
                "sparks-cluster".to_string(),
                measured_entry(
                    "sparks-cluster",
                    gents::BackendHealthState::Unhealthy,
                    3,
                    "2026-07-07T12:00:00Z",
                ),
            ),
            (
                "workstation-1".to_string(),
                measured_entry(
                    "workstation-1",
                    gents::BackendHealthState::Healthy,
                    0,
                    "2026-07-07T12:00:00Z",
                ),
            ),
            (
                "spark-2".to_string(),
                measured_entry(
                    "spark-2",
                    gents::BackendHealthState::Degraded,
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
            r#"gents_background_completion_pending_notifications{agent_did="did:agent:amy"} 3"#
        ));
        assert!(rendered.contains(
            r#"gents_background_completion_stranded_notifications{agent_did="did:agent:amy"} 2"#
        ));
        assert!(rendered.contains(
            r#"gents_background_completion_epochs{agent_did="did:agent:amy",state="exhausted"} 1"#
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
                    next_pending_retry_in_ms: Some(71),
                    pending_dag_terminal_quarantined: 73,
                    quarantined_pending_dags: 79,
                    ..Default::default()
                }),
                stale: false,
            }),
        );
        let rendered = lines.join("\n");
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
        assert!(rendered.contains("gents_p2p_single_flight_suppressed_total 37"));
        assert!(rendered.contains("gents_p2p_already_merged_fast_path_total 53"));
        assert!(rendered.contains("gents_p2p_pending_dag_terminal_quarantined_total 73"));
        assert!(rendered.contains(r#"gents_p2p_peer_consecutive_failures{peer_id="peer-a"} 3"#));
        assert!(rendered
            .contains(r#"gents_p2p_peer_cooldown_remaining_milliseconds{peer_id="peer-a"} 750"#));
    }

    #[test]
    fn p2p_metrics_render_stale_holds_last_known_counts() {
        let mut lines = Vec::new();
        render_p2p_metrics(
            &mut lines,
            Some(&P2pMetricsSnapshot {
                enabled: true,
                connected_peers: 7,
                replicators: 4,
                admission: None,
                sync_status: None,
                stale: true,
            }),
        );
        let rendered = lines.join("\n");
        assert!(rendered.contains("gents_p2p_status_stale 1"));
        assert!(rendered.contains("gents_p2p_connected_peers 7"));
        assert!(rendered.contains("gents_p2p_replicators 4"));
        assert!(rendered.contains("gents_p2p_sync_status_available 0"));
    }

    #[test]
    fn metrics_query_envelope_treats_null_probe_status_as_unknown() {
        let envelope: MetricsQueryEnvelope = serde_json::from_value(serde_json::json!({
            "AgentRuntime": [],
            "AgentBehaviorReadiness": [],
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

        assert!(body.contains("# TYPE gents_inference_metrics_window_seconds gauge"));
        assert!(body.contains("gents_inference_metrics_window_seconds 300"));
        assert!(body.contains("# TYPE gents_inference_requests_window_count gauge"));
        assert!(body.contains(
            "gents_inference_requests_window_count{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\",status=\"completed\"} 1"
        ));
        assert!(body.contains(
            "gents_inference_prompt_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 10"
        ));
        assert!(body.contains(
            "gents_inference_completion_tokens_window_sum{agent=\"agent \\\"friendly\\\"\",agent_did=\"did:key:zAgent\",backend_id=\"backend-1\",model=\"model\\none\"} 3"
        ));
    }
}
