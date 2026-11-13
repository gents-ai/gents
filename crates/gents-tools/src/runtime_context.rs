use std::path::PathBuf;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use gents_loop::live_output::LiveToolOutputWriter;
use gents_loop::tool::ToolCallEnvelope;
use gents_loop::tool_call_lifecycle::runtime::ToolScopeEntry;
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
    identity: Option<ToolRequestIdentity>,
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
                identity,
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

/// The per-request host values the tool surface reads. The default, every value
/// absent, is what a call outside any request sees.
#[derive(Clone, Default)]
pub struct ToolHostContext {
    pub workspace: ToolWorkspaceScope,
    pub interactive: bool,
    pub approval: Option<crate::plugin::approval::workspace::Context>,
}

fn ambient_host() -> ToolHostContext {
    ToolHostContext {
        workspace: WORKSPACE_OVERLAY.try_with(Clone::clone).unwrap_or_default(),
        interactive: crate::plugin::approval::interactive(),
        approval: crate::plugin::approval::workspace::current(),
    }
}

/// The host values of the call `envelope` serves.
#[track_caller]
pub fn host(envelope: &ToolCallEnvelope) -> ToolHostContext {
    if envelope.extensions().is_ambient() {
        return ambient_host();
    }
    let explicit = host_unchecked(envelope);
    debug_assert_same_host(&explicit, &ambient_host());
    explicit
}

fn host_unchecked(envelope: &ToolCallEnvelope) -> ToolHostContext {
    if envelope.extensions().is_ambient() {
        return ambient_host();
    }
    envelope
        .extensions()
        .get::<ToolHostContext>()
        .cloned()
        .unwrap_or_default()
}

/// Debug builds only: panics when explicit host values differ from the
/// ambient ones. Exists while their task-local statics do (#3765).
#[track_caller]
fn debug_assert_same_host(explicit: &ToolHostContext, ambient: &ToolHostContext) {
    if !cfg!(debug_assertions) {
        return;
    }
    let view = |host: &ToolHostContext| {
        (
            host.workspace.runtime_home.clone(),
            host.workspace.workspace_cwd.clone(),
            host.workspace.workspace_root.clone(),
            host.workspace.workspace_authority,
            host.workspace.workspace_artifact.is_some(),
            host.interactive,
            host.approval.is_some(),
        )
    };
    let same_approval = match (&explicit.approval, &ambient.approval) {
        (Some(explicit), Some(ambient)) => explicit.same_request(ambient),
        (None, None) => true,
        _ => false,
    };
    assert!(
        view(explicit) == view(ambient) && same_approval,
        "explicit tool host context differs from the ambient one: explicit {:?}, ambient {:?}",
        view(explicit),
        view(ambient)
    );
}

/// Debug builds only: panics when any explicit value of `envelope` differs
/// from its ambient one.
#[track_caller]
pub fn debug_assert_ambient(envelope: &ToolCallEnvelope) {
    if !cfg!(debug_assertions) {
        return;
    }
    envelope.debug_assert_ambient();
    let _ = host(envelope);
    let _ = gents_session::exec::ownership::process_recorder(envelope);
}

/// The placeholder envelope for a caller that has no envelope yet; every value
/// is the task-local one when read (#3765).
pub fn ambient_call_context() -> ToolCallEnvelope {
    ToolCallEnvelope::ambient()
}

/// The runtime context of the call `envelope` serves, with its workspace.
#[track_caller]
pub fn tool_runtime_context(envelope: &ToolCallEnvelope) -> Option<CurrentToolRuntimeContext> {
    let scope = envelope.runtime()?;
    Some(with_workspace(scope, Some(host(envelope).workspace)))
}

/// The envelope `scope_request_tool_execution_with_workspace_overlay` enters
/// inside `enclosing`. `entry.workspace_cwd` is replaced by the workspace's
/// working directory, or its root.
pub fn entered_with_workspace_overlay(
    enclosing: &ToolCallEnvelope,
    workspace: ToolWorkspaceScope,
    entry: ToolScopeEntry,
) -> ToolCallEnvelope {
    let mut entry = entry;
    entry.workspace_cwd = workspace
        .workspace_cwd
        .clone()
        .or_else(|| workspace.workspace_root.clone());
    let runtime = gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext::entered(
        enclosing.runtime_unchecked().as_ref(),
        entry,
    );
    let host = ToolHostContext {
        workspace,
        ..host_unchecked(enclosing)
    };
    enclosing
        .clone()
        .with_runtime(Some(runtime))
        .with_extension(host)
}

/// Runs `future` with every task-local static set from `envelope`, as the scope
/// entries that computed it would. A test bridge while the statics exist
/// (#3765).
pub async fn enter_ambient<F, T>(envelope: &ToolCallEnvelope, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let host = host_unchecked(envelope);
    let recorder = if envelope.extensions().is_ambient() {
        None
    } else {
        envelope
            .extensions()
            .get::<gents_session::exec::ownership::ProcessRecorder>()
            .cloned()
    };
    let runtime = envelope.runtime_unchecked();
    let future = gents_loop::tool_effects::scope(
        envelope.effects_unchecked(),
        gents_loop::tool_call_lifecycle::runtime::enter_ambient_runtime(runtime.as_ref(), future),
    );
    let future = WORKSPACE_OVERLAY.scope(host.workspace, future);
    let future = crate::plugin::approval::scope_interactive(host.interactive, future);
    let future = crate::plugin::approval::workspace::scope(host.approval, future);
    match recorder {
        Some(recorder) => {
            gents_session::exec::ownership::scope_process_recorder(recorder, future).await
        }
        None => future.await,
    }
}

fn with_workspace(
    scope: gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext,
    overlay: Option<ToolWorkspaceScope>,
) -> CurrentToolRuntimeContext {
    CurrentToolRuntimeContext {
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
    }
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
pub(crate) use gents_loop::tool_call_lifecycle::runtime::tool_execution_bounds;
pub use gents_loop::tool_call_lifecycle::runtime::ToolRequestIdentity;
#[cfg(test)]
pub(crate) use gents_loop::tool_call_lifecycle::runtime::{
    call_tool_managed, scope_request_tool_execution_with_trigger_context,
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
        None,
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
        None,
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
