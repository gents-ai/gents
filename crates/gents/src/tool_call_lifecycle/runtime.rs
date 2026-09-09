//! Native extension of the loop's tool-runtime scope (`gents-loop`, G-1).
//!
//! The loop's own `TOOL_RUNTIME_SCOPE` task-local (deadline, cancellation,
//! session id, live output, background/correlation/identity) moved to
//! `gents-loop`, since `agent/loop_stream/tool_dispatch.rs` sets it for every
//! dispatched tool call and no longer lives in this crate. Native, filesystem
//! and DefraDB-touching tools (bash, file, LSP) additionally need a workspace
//! binding — a cwd, a root, an authority level, and sometimes an artifact
//! write grant — which stays here as a second, nested task-local overlay
//! (`WORKSPACE_OVERLAY`). [`current_tool_runtime_context`] reads both and
//! merges them into one native-only context; the loop's own dispatch never
//! sees the overlay half.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use gents_loop::live_output::LiveToolOutputWriter;
use gents_loop::tool_policy::WorkspaceAuthority;

#[derive(Clone, Default)]
pub(crate) struct ToolWorkspaceScope {
    pub workspace_cwd: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub workspace_authority: Option<WorkspaceAuthority>,
    pub workspace_artifact: Option<crate::workspace::ArtifactGrant>,
}

impl ToolWorkspaceScope {
    pub(crate) fn cwd_only(workspace_cwd: Option<PathBuf>) -> Self {
        Self {
            workspace_cwd,
            workspace_root: None,
            workspace_authority: None,
            workspace_artifact: None,
        }
    }
}

tokio::task_local! {
    static WORKSPACE_OVERLAY: ToolWorkspaceScope;
}

/// The merged native context: everything the loop's own scope carries, plus
/// the workspace binding only native tools observe.
#[derive(Clone)]
pub(crate) struct CurrentToolRuntimeContext {
    pub(crate) deadline_at: Option<DateTime<Utc>>,
    pub(crate) cancellation_token: CancellationToken,
    pub(crate) workspace_cwd: Option<PathBuf>,
    pub(crate) workspace_root: Option<PathBuf>,
    pub(crate) workspace_authority: Option<WorkspaceAuthority>,
    pub(crate) workspace_artifact: Option<crate::workspace::ArtifactGrant>,
    pub(crate) session_id: Option<String>,
    pub(crate) live_output: Option<LiveToolOutputWriter>,
    pub(crate) background: bool,
    pub(crate) correlation: Option<String>,
    pub(crate) source_fields: std::collections::BTreeMap<String, String>,
    pub(crate) requester_did: Option<String>,
    pub(crate) agent_did: Option<String>,
    pub(crate) behavior_id: Option<String>,
    pub(crate) request_id: Option<String>,
}

/// Establish the workspace overlay around `future`, nesting the loop's own
/// scope inside it so a tool dispatched through `dispatch_tool` still sees
/// deadline/cancellation/session/live-output alongside the workspace binding.
pub(crate) async fn scope_request_tool_execution_with_workspace_overlay<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace: ToolWorkspaceScope,
    live_output: Option<LiveToolOutputWriter>,
    session_id: Option<String>,
    correlation: Option<String>,
    source_fields: std::collections::BTreeMap<String, String>,
    background: bool,
    future: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    let cwd = workspace
        .workspace_cwd
        .clone()
        .or_else(|| workspace.workspace_root.clone());
    WORKSPACE_OVERLAY
        .scope(
            workspace,
            gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                deadline_at,
                cancellation_token,
                cwd,
                live_output,
                session_id,
                correlation,
                source_fields,
                background,
                future,
            ),
        )
        .await
}

pub(crate) fn current_tool_runtime_context() -> Option<CurrentToolRuntimeContext> {
    let scope = gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()?;
    let overlay = WORKSPACE_OVERLAY.try_with(Clone::clone).ok();
    Some(CurrentToolRuntimeContext {
        deadline_at: scope.deadline_at,
        cancellation_token: scope.cancellation_token,
        workspace_cwd: scope.workspace_cwd,
        workspace_root: overlay
            .as_ref()
            .and_then(|overlay| overlay.workspace_root.clone()),
        workspace_authority: overlay
            .as_ref()
            .and_then(|overlay| overlay.workspace_authority),
        workspace_artifact: overlay
            .as_ref()
            .and_then(|overlay| overlay.workspace_artifact.clone()),
        session_id: scope.session_id,
        live_output: scope.live_output,
        background: scope.background,
        correlation: scope.correlation,
        source_fields: scope.source_fields,
        requester_did: scope.requester_did,
        agent_did: scope.agent_did,
        behavior_id: scope.behavior_id,
        request_id: scope.request_id,
    })
}

pub(crate) use gents_loop::tool_call_lifecycle::runtime::{
    call_tool_managed, scope_request_tool_execution_with_trigger_context,
    scope_tool_request_identity, tool_execution_bounds,
};
// Test-only: gents' own tool-execution tests exercise the loop's deadline
// arithmetic directly, and `scope_request_tool_execution_with_workspace`
// below (a test-only helper) still threads through the session scope; no
// non-test call site in this crate needs either any more (the dispatch loop
// that used to call them here moved to gents-loop).
#[cfg(test)]
pub(crate) use gents_loop::tool_call_lifecycle::runtime::{
    deadline_remaining, scope_request_tool_execution_with_session,
};

#[cfg(test)]
pub(crate) async fn scope_request_tool_execution<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    future: F,
) -> T
where
    F: std::future::Future<Output = T>,
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

#[cfg(test)]
pub(crate) async fn scope_request_tool_execution_with_workspace<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    future: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    scope_request_tool_execution_with_session(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        None,
        current_tool_runtime_context().and_then(|scope| scope.session_id),
        future,
    )
    .await
}

/// Scope for executions spawned through the R6 background bridge: identical
/// to the foreground scope except tools can observe `background` and apply
/// the background lifetime budget instead of their foreground ceiling.
#[cfg(test)]
pub(crate) async fn scope_background_tool_execution<F, T>(
    deadline_at: Option<DateTime<Utc>>,
    cancellation_token: CancellationToken,
    workspace_cwd: Option<PathBuf>,
    live_output: Option<LiveToolOutputWriter>,
    future: F,
) -> T
where
    F: std::future::Future<Output = T>,
{
    let inherited = current_tool_runtime_context();
    scope_request_tool_execution_with_trigger_context(
        deadline_at,
        cancellation_token,
        workspace_cwd,
        live_output,
        inherited
            .as_ref()
            .and_then(|scope| scope.session_id.clone()),
        inherited
            .as_ref()
            .and_then(|scope| scope.correlation.clone()),
        inherited
            .map(|scope| scope.source_fields)
            .unwrap_or_default(),
        true,
        future,
    )
    .await
}
