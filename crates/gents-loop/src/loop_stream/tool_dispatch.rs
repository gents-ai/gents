use super::*;

// pub, not pub(super): gents' own tool-execution tests exercise this
// formatting helper directly (a pub(super) item in this crate is invisible to
// a dependent crate's own test build).
pub fn value_to_json_string(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(string) => string.clone(),
        other => other.to_string(),
    }
}

/// Tool-policy admission, evaluated before the dispatch election. `Some` is
/// the pre-dispatch failure that settles the still-Pending call; the tool is
/// never invoked. Unknown names are left to `dispatch_tool`.
pub(super) fn admit_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: &str,
) -> Option<ToolOutcome> {
    let tool = tools.iter().find(|tool| tool.name() == name)?;
    tool.admit(args)
        .err()
        .map(|error| ToolOutcome::from_dispatch(name, Err(error)))
}

pub async fn dispatch_tool(
    tools: &[Box<dyn ToolDyn>],
    name: &str,
    args: String,
    live_output: Option<crate::live_output::LiveToolOutputWriter>,
    session_id: Option<String>,
) -> ToolOutcome {
    let Some(tool) = tools.iter().find(|tool| tool.name() == name) else {
        // An unresolved name is a typed dispatch failure, not completed output.
        return ToolOutcome::from_dispatch(
            name,
            Err(crate::tool::ToolError::ReportedFailure {
                class: crate::tool_call_lifecycle::FailureClass::ArgumentInvalid,
                text: format!("error: unknown tool '{name}'"),
            }),
        );
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
        result = call => {
            if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                ToolOutcome::TimedOut { deadline_at: scope.deadline_at }
            } else {
                ToolOutcome::from_dispatch(name, result)
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::{BoxFuture, ToolDefinition, ToolError};
    use crate::tool_call_lifecycle::runtime::scope_request_tool_execution;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    struct ReadyAfterWallDeadline {
        deadline_at: DateTime<Utc>,
        called: Arc<AtomicBool>,
    }

    impl ToolDyn for ReadyAfterWallDeadline {
        fn name(&self) -> String {
            "ready_after_wall_deadline".into()
        }

        fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "ready_after_wall_deadline".into(),
                    description: "wall-clock deadline regression".into(),
                    parameters: serde_json::json!({"type": "object"}),
                }
            })
        }

        fn call<'a>(&'a self, _args: String) -> BoxFuture<'a, Result<String, ToolError>> {
            Box::pin(async move {
                self.called.store(true, Ordering::SeqCst);
                let remaining = (self.deadline_at - Utc::now()).to_std().unwrap_or_default();
                // This synchronous call returns Ready on its first poll after
                // wall time advances. Tokio cannot repoll its sibling sleep
                // while the current task is inside this tool future.
                std::thread::sleep(remaining + std::time::Duration::from_millis(10));
                Ok("late success".into())
            })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ready_tool_after_wall_deadline_is_timed_out() {
        let deadline_at = Utc::now() + chrono::Duration::seconds(1);
        let called = Arc::new(AtomicBool::new(false));
        let tools: Vec<Box<dyn ToolDyn>> = vec![Box::new(ReadyAfterWallDeadline {
            deadline_at,
            called: Arc::clone(&called),
        })];
        let outcome = scope_request_tool_execution(
            Some(deadline_at),
            tokio_util::sync::CancellationToken::new(),
            dispatch_tool(&tools, "ready_after_wall_deadline", "{}".into(), None, None),
        )
        .await;
        assert!(
            called.load(Ordering::SeqCst),
            "tool must reach the call-ready branch"
        );
        assert_eq!(
            outcome,
            ToolOutcome::TimedOut {
                deadline_at: Some(deadline_at)
            }
        );
    }
}
