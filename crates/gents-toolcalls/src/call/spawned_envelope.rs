//! The envelopes of executions the hook moves into a task of their own. Such a
//! task starts empty: from its parent it keeps only the request's interactive
//! flag and approval context (Lean `CallEnvelope.request_constants_cross_spawn`),
//! and its owner supplies everything else. The parent is never the enclosing
//! context, so its dispatcher and process recorder do not cross the spawn.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gents_loop::live_output::LiveToolOutputWriter;
use gents_loop::tool::ToolCallEnvelope;
use gents_loop::tool_call_lifecycle::runtime::{ToolRequestIdentity, ToolScopeEntry};
use gents_loop::tool_effects::ToolEffectDispatcher;
use gents_session::exec::ownership::{with_process_recorder, ProcessRecorder};
use gents_tools::runtime_context::{
    entered_with_workspace_overlay, host, tool_runtime_context, ToolHostContext, ToolWorkspaceScope,
};
use tokio_util::sync::CancellationToken;

/// What the background owner admitted for one spawned execution.
pub(crate) struct BackgroundAdmission {
    pub request_id: String,
    pub deadline_at: DateTime<Utc>,
    pub cancellation_token: CancellationToken,
    pub live_output: LiveToolOutputWriter,
    pub session_id: String,
}

fn spawned_base(parent: &ToolCallEnvelope) -> ToolCallEnvelope {
    let parent_host = host(parent);
    ToolCallEnvelope::default().with_extension(ToolHostContext {
        workspace: ToolWorkspaceScope::default(),
        interactive: parent_host.interactive,
        approval: parent_host.approval,
    })
}

fn workspace_of(
    runtime: Option<&gents_tools::runtime_context::CurrentToolRuntimeContext>,
) -> ToolWorkspaceScope {
    ToolWorkspaceScope {
        runtime_home: runtime.and_then(|runtime| runtime.runtime_home.clone()),
        workspace_cwd: runtime.and_then(|runtime| runtime.workspace_cwd.clone()),
        workspace_root: runtime.and_then(|runtime| runtime.workspace_root.clone()),
        workspace_authority: runtime.and_then(|runtime| runtime.workspace_authority),
        workspace_artifact: runtime.and_then(|runtime| runtime.workspace_artifact.clone()),
    }
}

/// The envelope a `spawn_process` target runs under: the parent request's
/// identity, workspace, correlation and source fields with the admitted
/// lifetime, a fresh cancellation and the owner's recorder. A composing
/// target also gets the owner's `dispatcher`; a native process never does.
pub(crate) fn background_envelope(
    parent: &ToolCallEnvelope,
    admission: BackgroundAdmission,
    recorder: ProcessRecorder,
    dispatcher: Option<Arc<dyn ToolEffectDispatcher>>,
    process: bool,
) -> ToolCallEnvelope {
    let runtime = tool_runtime_context(parent);
    let entry = ToolScopeEntry {
        identity: Some(ToolRequestIdentity::new(
            runtime
                .as_ref()
                .and_then(|runtime| runtime.requester_did.clone()),
            runtime
                .as_ref()
                .and_then(|runtime| runtime.node_did.clone()),
            runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_did.clone()),
            Some(admission.request_id),
        )),
        deadline_at: Some(admission.deadline_at),
        cancellation_token: admission.cancellation_token,
        workspace_cwd: None,
        live_output: Some(admission.live_output),
        session_id: Some(admission.session_id),
        correlation: runtime
            .as_ref()
            .and_then(|runtime| runtime.correlation.clone()),
        source_fields: runtime
            .as_ref()
            .map(|runtime| runtime.source_fields.clone())
            .unwrap_or_default(),
        background: true,
    };
    let envelope = entered_with_workspace_overlay(
        &spawned_base(parent),
        workspace_of(runtime.as_ref()),
        entry,
    );
    let envelope = with_process_recorder(envelope, recorder);
    if process {
        envelope
    } else {
        envelope.with_effects(dispatcher)
    }
}

/// The envelope a foreground native process the background owner takes over
/// runs under: the parent's deadline, workspace, session, correlation and
/// source fields, the owner's fresh `process_token` and its recorder, and no
/// identity, which the foreground call's own scope supplies. `None` when the
/// parent has no request execution scope.
pub(crate) fn handoff_envelope(
    parent: &ToolCallEnvelope,
    process_token: CancellationToken,
    recorder: ProcessRecorder,
) -> Option<ToolCallEnvelope> {
    let runtime = tool_runtime_context(parent)?;
    let workspace = workspace_of(Some(&runtime));
    let entry = ToolScopeEntry {
        identity: None,
        deadline_at: runtime.deadline_at,
        cancellation_token: process_token,
        workspace_cwd: None,
        live_output: None,
        session_id: runtime.session_id,
        correlation: runtime.correlation,
        source_fields: runtime.source_fields,
        background: false,
    };
    let envelope = entered_with_workspace_overlay(&spawned_base(parent), workspace, entry);
    Some(with_process_recorder(envelope, recorder))
}

#[cfg(test)]
mod conformance_tests;
