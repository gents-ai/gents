pub(super) async fn log_recovery(
    node: &std::sync::Arc<defra_node::Node>,
    node_did: &str,
    default_agent_did: &str,
    agent_dids: &[String],
    executions: &crate::tools::call::hook::BackgroundExecutionRegistry,
) {
    // The inference-call sweep is parent-gated and must run after request repair.
    let outcome = crate::runtime::startup_recovery::run_startup_recovery_with_executions(
        node, node_did, executions,
    )
    .await;
    for agent_did in agent_dids {
        let result = match identity::Did::new(agent_did.clone()) {
            Ok(actor) => {
                crate::session::child_ownership::recover_agent_session_child_relationships(
                    node, node_did, &actor,
                )
                .await
            }
            Err(error) => Err(error.into()),
        };
        if let Err(error) = result {
            tracing::warn!(%agent_did, %error, "session child relationship recovery failed");
        }
        if let Ok(actor) = identity::Did::new(agent_did.clone()) {
            if let Err(error) =
                crate::session::recover_agent_session_relationships(node, node_did, &actor).await
            {
                tracing::warn!(%agent_did, %error, "session relationship recovery failed");
            }
        }
        if let Err(error) =
            crate::triggers::engine::durable::recover_outcomes(node, node_did, agent_did).await
        {
            tracing::warn!(%agent_did, %error, "agent trigger outcome recovery failed");
        }
    }
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
            default_agent_did = %default_agent_did,
            "startup recovery found no stuck documents"
        );
    }
}
