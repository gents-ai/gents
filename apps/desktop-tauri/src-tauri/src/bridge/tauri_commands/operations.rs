//! Tauri command stubs for operator-surfaces panels. Each command's body
//! is `unimplemented!()` until the named panel issue replaces it with the
//! real implementation. Until then no panel UI calls these — the stubs
//! exist so the panel PRs can be reviewed as additive replacements rather
//! than additive surface area + replacement combined.

use tauri::State;

use super::super::state::DesktopAppState;
use super::super::types::{
    CascadeCancelPreview, DesktopInterruptRequest, DesktopListSubagentTreeRequest,
    DesktopOperationsSnapshot, DesktopOperationsSnapshotRequest,
    DesktopPreviewInterruptCascadeRequest, InterruptRequestResult, SubagentTreeView,
};

#[tauri::command]
pub(crate) async fn desktop_operations_snapshot(
    _state: State<'_, DesktopAppState>,
    _request: DesktopOperationsSnapshotRequest,
) -> Result<DesktopOperationsSnapshot, String> {
    unimplemented!(
        "desktop_operations_snapshot is implemented by panel #277 (backgrounded tools) / \
         operations projection follow-up"
    )
}

#[tauri::command]
pub(crate) async fn desktop_list_subagent_tree(
    _state: State<'_, DesktopAppState>,
    _request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, String> {
    unimplemented!(
        "desktop_list_subagent_tree is implemented by panel #285 (subagent lineage view)"
    )
}

#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    _state: State<'_, DesktopAppState>,
    _request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    unimplemented!(
        "desktop_preview_interrupt_cascade is implemented by panel #286 (cascade cancel UX)"
    )
}

#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    _state: State<'_, DesktopAppState>,
    _request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    unimplemented!(
        "desktop_interrupt_request is implemented by panel #283 (interrupt button)"
    )
}
