//! Tauri command stubs for operator-surfaces panels. Each command returns
//! an `Err` describing the panel issue that will replace it; returning an
//! error rather than `unimplemented!()` keeps the desktop backend from
//! panicking if these are accidentally invoked before the real bodies
//! land. Panel PRs replace the body with the real implementation.

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
    Err(
        "desktop_operations_snapshot not implemented yet; landing via panel #277 \
         (backgrounded tools / operations projection)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_list_subagent_tree(
    _state: State<'_, DesktopAppState>,
    _request: DesktopListSubagentTreeRequest,
) -> Result<SubagentTreeView, String> {
    Err(
        "desktop_list_subagent_tree not implemented yet; landing via panel #285 \
         (subagent lineage view)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_preview_interrupt_cascade(
    _state: State<'_, DesktopAppState>,
    _request: DesktopPreviewInterruptCascadeRequest,
) -> Result<CascadeCancelPreview, String> {
    Err(
        "desktop_preview_interrupt_cascade not implemented yet; landing via panel #286 \
         (cascade cancel UX)"
            .to_string(),
    )
}

#[tauri::command]
pub(crate) async fn desktop_interrupt_request(
    _state: State<'_, DesktopAppState>,
    _request: DesktopInterruptRequest,
) -> Result<InterruptRequestResult, String> {
    Err(
        "desktop_interrupt_request not implemented yet; landing via panel #283 \
         (interrupt button)"
            .to_string(),
    )
}
