use std::path::PathBuf;

use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

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
}

/// The per-request host values the tool surface reads. The default, every value
/// absent, is what a call outside any request sees.
#[derive(Clone, Default)]
pub struct ToolHostContext {
    pub workspace: ToolWorkspaceScope,
    pub interactive: bool,
    pub approval: Option<crate::plugin::approval::workspace::Context>,
}

impl ToolHostContext {
    /// A call envelope that carries only these host values.
    pub fn call(self) -> ToolCallEnvelope {
        ToolCallEnvelope::default().with_extension(self)
    }
}

/// The host values of the call `envelope` serves.
pub fn host(envelope: &ToolCallEnvelope) -> ToolHostContext {
    envelope
        .extensions()
        .get::<ToolHostContext>()
        .cloned()
        .unwrap_or_default()
}

/// The runtime context of the call `envelope` serves, with its workspace.
pub fn tool_runtime_context(envelope: &ToolCallEnvelope) -> Option<CurrentToolRuntimeContext> {
    let scope = envelope.runtime()?;
    Some(with_workspace(scope, host(envelope).workspace))
}

/// The envelope of a call entered with `workspace` and `entry` inside
/// `enclosing`. `entry.workspace_cwd` is replaced by the workspace's working
/// directory, or its root.
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
        enclosing.runtime().as_ref(),
        entry,
    );
    let host = ToolHostContext {
        workspace,
        ..host(enclosing)
    };
    enclosing
        .clone()
        .with_runtime(Some(runtime))
        .with_extension(host)
}

fn with_workspace(
    scope: gents_loop::tool_call_lifecycle::runtime::CurrentToolRuntimeContext,
    overlay: ToolWorkspaceScope,
) -> CurrentToolRuntimeContext {
    CurrentToolRuntimeContext {
        runtime_home: overlay.runtime_home,
        deadline_at: scope.deadline_at,
        cancellation_token: scope.cancellation_token,
        workspace_cwd: scope.workspace_cwd,
        workspace_root: overlay.workspace_root,
        workspace_authority: overlay.workspace_authority,
        workspace_artifact: overlay.workspace_artifact,
        session_id: scope.session_id,
        background: scope.background,
        correlation: scope.correlation,
        source_fields: scope.source_fields,
        requester_did: scope.requester_did,
        node_did: scope.node_did,
        agent_did: scope.agent_did,
    }
}

#[cfg(test)]
pub(crate) use gents_loop::tool_call_lifecycle::runtime::call_tool_managed;
pub(crate) use gents_loop::tool_call_lifecycle::runtime::tool_execution_bounds;
pub use gents_loop::tool_call_lifecycle::runtime::ToolRequestIdentity;
