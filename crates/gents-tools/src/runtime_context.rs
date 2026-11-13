use std::path::PathBuf;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use gents_loop::live_output::LiveToolOutputWriter;
use gents_loop::tool_policy::WorkspaceAuthority;

#[derive(Clone, Default)]
pub struct ToolWorkspaceScope {
    /// Host namespace for negative credential exclusions, never portable identity
    /// or authority to read/write the runtime home.
    pub runtime_home: Option<PathBuf>,
    pub workspace_cwd: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub workspace_authority: Option<WorkspaceAuthority>,
    pub workspace_artifact: Option<gents_session::workspace::ArtifactGrant>,
}

impl ToolWorkspaceScope {
    pub fn cwd_only(workspace_cwd: Option<PathBuf>) -> Self {
        Self {
            runtime_home: None,
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

#[derive(Clone)]
pub struct CurrentToolRuntimeContext {
    pub runtime_home: Option<PathBuf>,
    pub deadline_at: Option<DateTime<Utc>>,
    pub cancellation_token: CancellationToken,
    pub workspace_cwd: Option<PathBuf>,
    pub workspace_root: Option<PathBuf>,
    pub workspace_authority: Option<WorkspaceAuthority>,
    pub workspace_artifact: Option<gents_session::workspace::ArtifactGrant>,
    pub session_id: Option<String>,
    pub(crate) background: bool,
    pub correlation: Option<String>,
    pub source_fields: std::collections::BTreeMap<String, String>,
    pub requester_did: Option<String>,
    pub node_did: Option<String>,
    pub agent_did: Option<String>,
    pub(crate) request_id: Option<String>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "mirrors the gents-loop scope entry point; called from toolset, hook, plugin and workspace tests"
)]
pub async fn scope_request_tool_execution_with_workspace_overlay<F, T>(
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

pub fn current_tool_runtime_context() -> Option<CurrentToolRuntimeContext> {
    let scope = gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()?;
    let overlay = WORKSPACE_OVERLAY.try_with(Clone::clone).ok();
    Some(CurrentToolRuntimeContext {
        runtime_home: overlay
            .as_ref()
            .and_then(|overlay| overlay.runtime_home.clone()),
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
        background: scope.background,
        correlation: scope.correlation,
        source_fields: scope.source_fields,
        requester_did: scope.requester_did,
        node_did: scope.node_did,
        agent_did: scope.agent_did,
        request_id: scope.request_id,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
pub use gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_session;
pub(crate) use gents_loop::tool_call_lifecycle::runtime::{
    call_tool_managed, call_tool_managed_with_cleanup,
    scope_request_tool_execution_with_trigger_context, scope_tool_request_identity,
    tool_execution_bounds,
};

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn scope_request_tool_execution<F, T>(
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

#[cfg(any(test, feature = "test-fixtures"))]
pub async fn scope_request_tool_execution_with_workspace<F, T>(
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

