use super::*;

pub(super) fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

pub(crate) async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::background_tools::LiveToolOutputWriter>,
    session_id: Option<String>,
) -> ToolOutcome {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        // An unresolved name is a typed dispatch failure, not completed output.
        return ToolOutcome::from_tool_call_error(&format!("error: unknown tool '{name}'"));
    };

    let Some(scope) = current_tool_runtime_context() else {
        return ToolOutcome::from_dispatch(name, tool.call(args).await);
    };

    if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
        return ToolOutcome::TimedOut {
            deadline_at: scope.deadline_at,
        };
    }

    let deadline_at = scope.deadline_at;
    let mut deadline = Box::pin(async move {
        match deadline_remaining(deadline_at) {
            Some(remaining) => tokio::time::sleep(remaining).await,
            None => std::future::pending::<()>().await,
        }
    });

    let call = scope_request_tool_execution_with_session(
        scope.deadline_at,
        scope.cancellation_token.clone(),
        scope.workspace_cwd.clone(),
        live_output,
        session_id.or(scope.session_id.clone()),
        tool.call(args),
    );
    tokio::select! {
        biased;
        _ = scope.cancellation_token.cancelled() => ToolOutcome::Cancelled,
        _ = &mut deadline => ToolOutcome::TimedOut { deadline_at: scope.deadline_at },
        result = call => ToolOutcome::from_dispatch(name, result),
    }
}
