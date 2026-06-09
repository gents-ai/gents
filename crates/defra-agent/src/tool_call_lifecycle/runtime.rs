//! Runtime enforcement bridge for tool-call lifecycle outcomes.
//!
//! Rig executes tools inside the stream future, while lifecycle persistence is
//! driven by hooks before and after that execution. This module installs a
//! request-scoped runtime context around stream polling and wraps every tool so
//! deadline/cancellation outcomes become explicit tool results that the hook
//! can map to `timedOut` / `cancelled` terminal states.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use chrono::{DateTime, Utc};
use rig::completion::ToolDefinition;
use rig::tool::{ToolDyn, ToolError};
use rig::wasm_compat::WasmBoxedFuture;
use tokio_util::sync::CancellationToken;

use crate::background_tools::LiveToolOutputWriter;

const MARKER_PREFIX: &str = "__defra_agent_tool_lifecycle__:";
const TIMEOUT_MARKER: &str = "__defra_agent_tool_lifecycle__:timedOut";
const CANCELLED_MARKER: &str = "__defra_agent_tool_lifecycle__:cancelled";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ManagedToolTerminal {
    TimedOut,
    Cancelled,
}

#[derive(Clone)]
struct ToolRuntimeScope {
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
}

#[derive(Clone)]
pub(crate) struct CurrentToolRuntimeContext {
    pub(crate) deadline_at: Option<DateTime<Utc>>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) workspace_cwd: Option<PathBuf>,
    pub(crate) live_output: Option<LiveToolOutputWriter>,
}

tokio::task_local! {
    static TOOL_RUNTIME_SCOPE: ToolRuntimeScope;
}

#[cfg(test)]
pub(crate) async fn scope_request_tool_execution<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    let workspace_cwd = current_tool_runtime_context().and_then(|scope| scope.workspace_cwd);
    scope_request_tool_execution_with_workspace(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        future,
    )
    .await
}

pub(crate) async fn scope_request_tool_execution_with_workspace<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    scope_request_tool_execution_with_workspace_and_live_output(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        None,
        future,
    )
    .await
}

pub(crate) async fn scope_request_tool_execution_with_workspace_and_live_output<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    TOOL_RUNTIME_SCOPE
        .scope(
            ToolRuntimeScope {
                deadline_at,
                cancellation_token,
                workspace_cwd,
                live_output,
            },
            future,
        )
        .await
}

pub(crate) fn wrap_tool(tool: Box<dyn ToolDyn>) -> Box<dyn ToolDyn> {
    Box::new(RuntimeManagedTool { inner: tool })
}

pub(crate) fn current_tool_runtime_context() -> Option<CurrentToolRuntimeContext> {
    TOOL_RUNTIME_SCOPE
        .try_with(Clone::clone)
        .ok()
        .map(|scope| CurrentToolRuntimeContext {
            deadline_at: scope.deadline_at,
            cancellation_token: scope.cancellation_token,
            workspace_cwd: scope.workspace_cwd,
            live_output: scope.live_output,
        })
}

pub(crate) fn classify_managed_tool_result(result: &str) -> Option<ManagedToolTerminal> {
    if !result.starts_with(MARKER_PREFIX) {
        return None;
    }
    if result.starts_with(TIMEOUT_MARKER) {
        return Some(ManagedToolTerminal::TimedOut);
    }
    if result.starts_with(CANCELLED_MARKER) {
        return Some(ManagedToolTerminal::Cancelled);
    }
    None
}

pub(crate) fn timeout_result(deadline_at: Option<DateTime<Utc>>) -> String {
    match deadline_at {
        Some(deadline_at) => format!("{TIMEOUT_MARKER}:{}", deadline_at.to_rfc3339()),
        None => TIMEOUT_MARKER.to_string(),
    }
}

pub(crate) fn cancelled_result() -> String {
    CANCELLED_MARKER.to_string()
}

struct RuntimeManagedTool {
    inner: Box<dyn ToolDyn>,
}

impl ToolDyn for RuntimeManagedTool {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn definition<'a>(&'a self, prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
        self.inner.definition(prompt)
    }

    fn call<'a>(&'a self, args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let Some(scope) = TOOL_RUNTIME_SCOPE.try_with(Clone::clone).ok() else {
                return self.inner.call(args).await;
            };

            if deadline_remaining(scope.deadline_at).is_some_and(|remaining| remaining.is_zero()) {
                return Ok(timeout_result(scope.deadline_at));
            }

            let mut deadline = Box::pin(async move {
                match deadline_remaining(scope.deadline_at) {
                    Some(remaining) => tokio::time::sleep(remaining).await,
                    None => std::future::pending::<()>().await,
                }
            });

            // Apply the request's deadline/cancellation envelope. Result bounding
            // is owned by the completion loop for foreground tools (#400); this
            // wrapper now only guards background tool execution.
            tokio::select! {
                biased;
                _ = scope.cancellation_token.cancelled() => Ok(cancelled_result()),
                _ = &mut deadline => Ok(timeout_result(scope.deadline_at)),
                result = self.inner.call(args) => result,
            }
        })
    }
}

fn deadline_remaining(deadline_at: Option<DateTime<Utc>>) -> Option<Duration> {
    let deadline_at = deadline_at?;
    let now = Utc::now();
    if now >= deadline_at {
        return Some(Duration::ZERO);
    }
    Some((deadline_at - now).to_std().unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rig::completion::ToolDefinition;

    struct PendingTool;

    impl ToolDyn for PendingTool {
        fn name(&self) -> String {
            "pending".to_string()
        }

        fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "pending".to_string(),
                    description: "test tool".to_string(),
                    parameters: serde_json::json!({"type":"object"}),
                }
            })
        }

        fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
            Box::pin(std::future::pending())
        }
    }

    struct FastTool;

    impl ToolDyn for FastTool {
        fn name(&self) -> String {
            "fast".to_string()
        }

        fn definition<'a>(&'a self, _prompt: String) -> WasmBoxedFuture<'a, ToolDefinition> {
            Box::pin(async {
                ToolDefinition {
                    name: "fast".to_string(),
                    description: "test tool".to_string(),
                    parameters: serde_json::json!({"type":"object"}),
                }
            })
        }

        fn call<'a>(&'a self, _args: String) -> WasmBoxedFuture<'a, Result<String, ToolError>> {
            Box::pin(async { Ok("ok".to_string()) })
        }
    }

    #[test]
    fn managed_result_markers_classify_terminal_outcomes() {
        assert_eq!(
            classify_managed_tool_result(&timeout_result(Some(Utc::now()))),
            Some(ManagedToolTerminal::TimedOut)
        );
        assert_eq!(
            classify_managed_tool_result(&cancelled_result()),
            Some(ManagedToolTerminal::Cancelled)
        );
        assert_eq!(classify_managed_tool_result("ordinary output"), None);
    }

    #[tokio::test]
    async fn wrapped_tool_times_out_at_request_deadline() {
        let tool = wrap_tool(Box::new(PendingTool));
        let deadline = Utc::now() + chrono::Duration::milliseconds(10);

        let result = scope_request_tool_execution(
            Some(deadline),
            CancellationToken::new(),
            tool.call("{}".to_string()),
        )
        .await
        .expect("timeout is returned as managed terminal output");

        assert_eq!(
            classify_managed_tool_result(&result),
            Some(ManagedToolTerminal::TimedOut)
        );
    }

    #[tokio::test]
    async fn wrapped_tool_cancels_before_inner_future_completes() {
        let tool = wrap_tool(Box::new(PendingTool));
        let token = CancellationToken::new();
        token.cancel();

        let result = scope_request_tool_execution(None, token, tool.call("{}".to_string()))
            .await
            .expect("cancel is returned as managed terminal output");

        assert_eq!(
            classify_managed_tool_result(&result),
            Some(ManagedToolTerminal::Cancelled)
        );
    }

    #[tokio::test]
    async fn wrapped_tool_preserves_fast_success() {
        let tool = wrap_tool(Box::new(FastTool));
        let deadline = Utc::now() + chrono::Duration::seconds(1);

        let result = scope_request_tool_execution(
            Some(deadline),
            CancellationToken::new(),
            tool.call("{}".to_string()),
        )
        .await
        .expect("fast tool should complete");

        assert_eq!(result, "ok");
        assert_eq!(classify_managed_tool_result(&result), None);
    }

}
