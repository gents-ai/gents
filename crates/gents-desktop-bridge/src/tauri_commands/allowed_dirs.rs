//! The "Allowed folders" setting and the plugin access questions: the same
//! list `gents plugin dirs` edits and the same questions `gents chat` answers,
//! through `gents::plugin::{allowed, approval}`, in the managed local Node's
//! home.

use gents::pack::BindAccess;
use gents::plugin::{allowed, approval};
use serde::Serialize;
use tauri::{Runtime, State, WebviewWindow};
use ts_rs::TS;

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

#[derive(Debug, Serialize, TS)]
pub struct AllowedFoldersView {
    pub dirs: Vec<allowed::AllowedDir>,
}

#[derive(Debug, Serialize, TS)]
pub struct PluginApprovalsView {
    pub requests: Vec<PluginApprovalView>,
}

#[derive(Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginApprovalView {
    pub id: String,
    pub prompt: String,
    pub folder: std::path::PathBuf,
    pub path: String,
    pub is_dir: bool,
    pub session_id: Option<String>,
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

/// The questions running plugin calls are waiting on, oldest first.
#[tauri::command]
pub fn desktop_plugin_approvals_pending<R: Runtime>(
    window: WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
) -> Result<PluginApprovalsView, BridgeError> {
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
        requests: pending
            .iter()
            .map(|request| PluginApprovalView {
                id: request.id.clone(),
                prompt: request.prompt(),
                folder: request.folder.clone(),
                path: request.path.clone(),
                is_dir: request.is_dir,
                session_id: request.session_id.clone(),
            })
            .collect(),
    })
}

/// Answers one question: `once`, `file` (also allows that exact file from now
/// on), `always` (also allows its folder from now on) or `deny`.
#[tauri::command]
pub fn desktop_plugin_approval_decide(
    id: String,
    decision: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
    let answer = match decision.as_str() {
        "once" => approval::Answer::Once,
        "file" => approval::Answer::AlwaysPath,
        "always" => approval::Answer::AlwaysFolder,
        "deny" => approval::Answer::Deny,
        other => {
            return Err(BridgeError::untyped(format!(
                "{other:?} is not an answer; use once, file, always or deny"
            )))
        }
    };
    approval::decide(&home(&state)?, &id, answer).map_err(failed)
}

#[tauri::command]
pub fn desktop_plugin_approval_subscribe<R: Runtime>(
    window: WebviewWindow<R>,
    token: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
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
pub fn desktop_plugin_approval_unsubscribe<R: Runtime>(
    window: WebviewWindow<R>,
    token: String,
    state: State<'_, DesktopAppState>,
) -> Result<(), BridgeError> {
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
