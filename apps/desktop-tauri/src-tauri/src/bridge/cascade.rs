//! Descendant tree walk for cascade preview and cascade interrupt.
//!
//! Mirrors `interrupt_request_local` in
//! `crates/defra-agent-cli/src/commands/subagent.rs:327`, but stays in the
//! bridge so both `desktop_preview_interrupt_cascade` and
//! `desktop_interrupt_request` can share the walk.

use defra_agent_desktop_core::client::ClientCore;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRequest {
    pub root_request_id: String,
    pub agent_did: Option<String>,
    pub include_terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CascadeClassification {
    WillInterrupt,
    WillDetach,
    AlreadyTerminal,
    UnknownPolicy,
}

#[derive(Debug, Clone)]
pub(crate) struct CascadeWalkRow {
    pub request_id: String,
    pub session_id: Option<String>,
    pub behavior_id: Option<String>,
    pub lifecycle_state: Option<String>,
    pub parent_request_id: Option<String>,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub await_mode: Option<String>,
    pub cancel_policy: Option<String>,
    pub classification: CascadeClassification,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct CascadeWalkResult {
    pub root_state: Option<String>,
    pub root_interrupt_requested_at: Option<String>,
    pub rows: Vec<CascadeWalkRow>,
}

/// Walks `AgentToolCall.child_request_id` edges from `root_request_id` down,
/// classifying each descendant by the nearest bridge row's `cancel_policy`.
/// Filters terminal rows when `include_terminal == false`, except as
/// AlreadyTerminal evidence.
pub(crate) async fn walk(
    _core: &Arc<ClientCore>,
    _req: &CascadeWalkRequest,
) -> Result<CascadeWalkResult, String> {
    // Real impl lands in Task 2.
    Err("cascade::walk not implemented yet".into())
}
