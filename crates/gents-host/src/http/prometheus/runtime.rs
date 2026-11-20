use gents_protocol::row::{
    decode_node_readiness_snapshot, project_node_readiness_summary, NodeReadinessRow,
    ProjectedNodeReadinessSummary,
};

use super::exposition::{push_metric_prelude, push_metric_sample, rfc3339_timestamp_seconds};
use super::store::MetricsQueryData;

pub(super) fn render_runtime_metrics(lines: &mut Vec<String>, data: &MetricsQueryData) {
    push_metric_prelude(
        lines,
        "gents_runtime_process_state",
        "One-hot process lifecycle state for each node runtime.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_reconcile_phase",
        "One-hot reconcile phase for each node runtime.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_last_reconcile_result",
        "One-hot last reconcile result for each node runtime.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_active_generation",
        "Current active runtime generation.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_router_generation",
        "Current router-observed runtime generation.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_runnable_agents",
        "Number of ready agents in the authoritative agent-readiness snapshot.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_unavailable_agents",
        "Number of unavailable agents in the authoritative agent-readiness snapshot.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_node_readiness_observed",
        "Whether the authoritative agent-readiness snapshot is valid and current.",
    );
    push_metric_prelude(
        lines,
        "gents_runtime_last_reconcile_completed_at_seconds",
        "Unix timestamp of the last completed reconcile.",
    );

    for readiness in runtime_inventory(data) {
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
                lines,
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
                    lines,
                    "gents_runtime_reconcile_phase",
                    &[("node_did", node_did.clone()), ("phase", phase.to_string())],
                    i64::from(runtime.reconcile_phase == phase),
                );
            }
            for result in ["startup", "noop", "applied", "error"] {
                push_metric_sample(
                    lines,
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
            lines,
            "gents_runtime_active_generation",
            &[("node_did", node_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.active_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_metric_sample(
            lines,
            "gents_runtime_router_generation",
            &[("node_did", node_did.clone())],
            lifecycle
                .as_ref()
                .map(|snapshot| snapshot.router_generation)
                .and_then(|generation| i64::try_from(generation).ok())
                .unwrap_or_default(),
        );
        push_node_readiness_metrics(lines, &node_did, &data.node_readiness);
        if let Some(timestamp) = runtime
            .and_then(|runtime| rfc3339_timestamp_seconds(&runtime.last_reconcile_completed_at))
        {
            push_metric_sample(
                lines,
                "gents_runtime_last_reconcile_completed_at_seconds",
                &[("node_did", node_did)],
                timestamp,
            );
        }
    }
}

fn runtime_inventory(data: &MetricsQueryData) -> impl Iterator<Item = &NodeReadinessRow> {
    data.node_readiness.iter()
}

fn push_node_readiness_metrics(lines: &mut Vec<String>, node_did: &str, rows: &[NodeReadinessRow]) {
    push_node_readiness_metrics_at(lines, node_did, rows, chrono::Utc::now());
}

pub(super) fn push_node_readiness_metrics_at(
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
