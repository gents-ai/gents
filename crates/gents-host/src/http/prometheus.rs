use gents_store::config_client::GraphqlEndpoint;
use rapidhash::RapidHashMap;

use anyhow::Result;

use exposition::{push_metric_prelude, push_metric_prelude_with_type, push_metric_sample};

mod backends;
mod background;
mod exposition;
mod inference;
mod inference_query;
pub mod p2p;
mod p2p_sync;
mod process;
mod runtime;
pub mod store;
#[cfg(test)]
mod tests;

pub use process::{sample_process_memory, ProcessMetricsSnapshot, StoreSizeCache};

pub async fn render_prometheus_metrics(
    graphql: &GraphqlEndpoint,
    local_node_did: &str,
    measured_backend_health: &RapidHashMap<
        String,
        gents_inference::backends::health::BackendHealthSnapshot,
    >,
    p2p: Option<&p2p::P2pMetricsSnapshot>,
    process: Option<&ProcessMetricsSnapshot>,
) -> Result<String> {
    let data = store::load_scrape_metrics_core_data(graphql, local_node_did)
        .await?
        .with_activity_reader(
            graphql,
            tokio::time::Instant::now() + store::LIVENESS_ACTIVITY_BUDGET,
            store::ActivityReader::Scrape,
        )
        .await;
    let data = store::with_local_native_executors(data);
    let inference_metrics = inference_query::load_inference_metrics_query_data(graphql).await?;
    let background_completion =
        gents_toolcalls::background::diagnostics::load_background_completion_diagnostics(
            &gents_store::config_client::ConfigAccess::Graphql(graphql.clone()),
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

    runtime::render_runtime_metrics(&mut lines, &data);

    backends::render_backend_metrics(
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

    inference::render_inference_metrics(&mut lines, &inference_metrics);
    render_command_policy_metrics(
        &mut lines,
        gents_tools::native::command_observability::snapshot(),
    );
    background::render_background_completion_metrics(
        &mut lines,
        local_node_did,
        background_completion.as_ref(),
    );
    p2p::render_p2p_metrics(&mut lines, p2p);
    process::render_process_metrics(&mut lines, process);

    lines.push(String::new());
    Ok(lines.join("\n"))
}

fn render_command_policy_metrics(
    lines: &mut Vec<String>,
    snapshot: gents_tools::native::command_observability::CommandPolicyMetrics,
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
