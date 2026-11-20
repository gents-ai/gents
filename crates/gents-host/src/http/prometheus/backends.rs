use rapidhash::RapidHashMap;

use super::exposition::{push_metric_prelude, push_metric_sample, rfc3339_timestamp_seconds};
use super::store::MetricsBackendRow;

pub(super) fn render_backend_metrics(
    lines: &mut Vec<String>,
    backends: &[MetricsBackendRow],
    measured_backend_health: &RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
) {
    push_metric_prelude(
        lines,
        "gents_backend_enabled",
        "Whether an inference backend is enabled.",
    );
    push_metric_prelude(
        lines,
        "gents_backend_max_concurrent",
        "Configured maximum concurrency for an inference backend.",
    );
    push_metric_prelude(
        lines,
        "gents_backend_max_queue_depth",
        "Configured admission queue depth for an inference backend.",
    );
    push_metric_prelude(
        lines,
        "gents_backend_probe_status",
        "Current probe status for an inference backend.",
    );
    push_metric_prelude(
        lines,
        "gents_backend_last_probe_seconds",
        "Unix timestamp of the last backend probe.",
    );

    push_backend_metrics(lines, backends, measured_backend_health);
}

pub fn push_backend_metrics(
    lines: &mut Vec<String>,
    backends: &[MetricsBackendRow],
    measured: &RapidHashMap<String, gents_inference::backends::health::BackendHealthSnapshot>,
) {
    for backend in backends {
        let observation = measured
            .get(&backend.backend_id)
            .map(|entry| (entry.state, entry.last_probe_at.timestamp()));
        push_backend_metric(lines, backend, observation);
    }
}

pub(super) fn push_backend_metric(
    lines: &mut Vec<String>,
    backend: &MetricsBackendRow,
    measured: Option<(gents_inference::backends::health::BackendHealthState, i64)>,
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
            .unwrap_or(gents_documents::inference_defaults::DEFAULT_MAX_CONCURRENT),
    );
    push_metric_sample(
        lines,
        "gents_backend_max_queue_depth",
        &[("backend_id", backend.backend_id.clone())],
        backend
            .max_queue_depth
            .unwrap_or(gents_documents::inference_defaults::DEFAULT_MAX_QUEUE_DEPTH),
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
