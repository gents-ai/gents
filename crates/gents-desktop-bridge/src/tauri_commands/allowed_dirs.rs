pub use crate::plugin_access::{AllowedFoldersView, PluginApprovalView, PluginApprovalsView};
use gents::pack::BindAccess;
use gents::plugin::{allowed, approval};
use tauri::{Runtime, State, WebviewWindow};

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "allowed folders belong to a local Node; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

fn view(home: &std::path::Path) -> Result<AllowedFoldersView, BridgeError> {
    Ok(AllowedFoldersView {
        dirs: allowed::list(home).map_err(failed)?,
    })
}

#[tauri::command]
pub fn desktop_allowed_dirs_list(
    state: State<'_, DesktopAppState>,
) -> Result<AllowedFoldersView, BridgeError> {
    view(&home(&state)?)
}

#[tauri::command]
pub fn desktop_allowed_dirs_add(
    path: String,
    access: String,
    state: State<'_, DesktopAppState>,
) -> Result<AllowedFoldersView, BridgeError> {
    let home = home(&state)?;
    let access: BindAccess = access.parse().map_err(failed)?;
    allowed::add(&home, std::path::Path::new(&path), access).map_err(failed)?;
    view(&home)
}

#[tauri::command]
pub fn desktop_allowed_dirs_remove(
    path: String,
    state: State<'_, DesktopAppState>,
) -> Result<AllowedFoldersView, BridgeError> {
    let home = home(&state)?;
    allowed::remove(&home, std::path::Path::new(&path)).map_err(failed)?;
    view(&home)
}

#[tauri::command]
pub fn desktop_plugin_approvals_pending<R>(
    window: WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<PluginApprovalsView, BridgeError>
where
    R: Runtime,
{
    let subscribed = state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .approvals_prompt_subscriptions
        .contains_window(window.label());
    let pending = if subscribed {
        approval::pending_requests(&home(&state)?).map_err(failed)?
    } else {
        Vec::new()
    };
    Ok(PluginApprovalsView {
        requests: pending.iter().map(PluginApprovalView::from).collect(),
    })
}

#[tauri::command]
pub fn desktop_plugin_approval_decide(
    id: String,
    decision: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let answer = crate::plugin_access::answer(&decision)?;
    approval::decide(&home(&state)?, &id, answer).map_err(failed)
}

#[tauri::command]
pub fn desktop_plugin_approval_subscribe<R>(
    window: WebviewWindow<R>,
    token: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    home(&state)?;
    let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
    if bridge.has_window(window.label()) {
        bridge
            .approvals_prompt_subscriptions
            .subscribe(window.label(), token);
    }
    Ok(())
}

#[tauri::command]
pub fn desktop_plugin_approval_unsubscribe<R>(
    window: WebviewWindow<R>,
    token: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError>
where
    R: Runtime,
{
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .approvals_prompt_subscriptions
        .unsubscribe(window.label(), &token);
    Ok(())
}

#[cfg(test)]
#[path = "allowed_dirs_tests.rs"]
mod tests;
