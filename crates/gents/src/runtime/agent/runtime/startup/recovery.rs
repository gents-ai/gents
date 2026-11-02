pub(super) async fn log_recovery(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    default_agent_id: &str,
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
) {
    // The inference-call sweep is parent-gated and must run after request repair.
    let outcome = crate::runtime::startup_recovery::run_startup_recovery_with_executions(
        node, node_did, executions,
    )
    .await;
    let mut recovered_any = false;

    match outcome.tool_calls {
        Ok(report) => {
            if report.tool_calls_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    count = report.tool_calls_recovered,
                    "recovered stuck tool calls"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                node_did = %node_did,
                error = %error,
                "startup tool-call recovery failed"
            );
        }
    }

    match outcome.inference_calls {
        Ok(report) => {
            if report.calls_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    count = report.calls_recovered,
                    "recovered stale inference calls"
                );
            }
        }
        Err(error) => {
            tracing::warn!(
                node_did = %node_did,
                error = %error,
                "startup inference-call recovery failed"
            );
        }
    }

    match outcome.task_hooks {
        Ok(report) => {
            if report > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    records = report,
                    "recovering interrupted task hook cleanup"
                );
            }
        }
        Err(error) => {
            tracing::warn!(node_did = %node_did, error = %error, "startup task hook recovery failed");
        }
    }

    match outcome.requests {
        Ok(report) => {
            if report.requests_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    count = report.requests_recovered,
                    "recovered stuck requests"
                );
            }
            if report.background_wakes_redriven > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    count = report.background_wakes_redriven,
                    "redrove failed background-completion wakes"
                );
            }
            if report.responses_recovered > 0 {
                recovered_any = true;
                tracing::info!(
                    node_did = %node_did,
                    count = report.responses_recovered,
                    "recovered stuck responses"
                );
            }
        }
        Err(error) => {
            tracing::warn!(node_did = %node_did, error = %error, "startup recovery failed");
        }
    }

    if !recovered_any {
        tracing::debug!(
            node_did = %node_did,
            default_agent_id = %default_agent_id,
            "startup recovery found no stuck documents"
        );
    }
}
