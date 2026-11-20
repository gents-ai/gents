use chrono::Utc;

use super::exposition::{push_metric_prelude, push_metric_sample};
use super::p2p_sync;

#[derive(Debug, Clone, Default)]
pub struct P2pMetricsSnapshot {
    pub enabled: bool,
    pub connected_peers: usize,
    pub replicators: usize,
    pub admission: Option<gents_service::state::P2pAdmissionState>,
    pub sync_status: Option<gents_p2p::p2p::observability::P2pSyncStatusSnapshot>,
    pub stale: bool,
    pub peer_id: Option<String>,
    pub transport: Option<String>,
}

pub(super) fn render_p2p_metrics(lines: &mut Vec<String>, p2p: Option<&P2pMetricsSnapshot>) {
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

    p2p_sync::render_p2p_sync_metrics(lines, snapshot.sync_status.as_ref());
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
