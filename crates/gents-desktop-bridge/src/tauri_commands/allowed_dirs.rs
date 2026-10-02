//! The "Allowed folders" setting and the plugin access questions: the same
//! list `gents plugin dirs` edits and the same questions `gents chat` answers,
//! through `gents::plugin::{allowed, approval}`, in the managed local agent's
//! home.

use gents::pack::BindAccess;
use gents::plugin::{allowed, approval};
use serde_json::{json, Value};
use tauri::State;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "allowed folders belong to a local agent; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

fn view(home: &std::path::Path) -> Result<Value, BridgeError> {
    Ok(json!({ "dirs": allowed::list(home).map_err(failed)? }))
}

#[tauri::command]
pub fn desktop_allowed_dirs_list(state: State<'_, DesktopAppState>) -> Result<Value, BridgeError> {
    view(&home(&state)?)
}

#[tauri::command]
pub fn desktop_allowed_dirs_add(
    path: String,
    access: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    let access: BindAccess = access.parse().map_err(failed)?;
    allowed::add(&home, std::path::Path::new(&path), access).map_err(failed)?;
    view(&home)
}

#[tauri::command]
pub fn desktop_allowed_dirs_remove(
    path: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    allowed::remove(&home, std::path::Path::new(&path)).map_err(failed)?;
    view(&home)
}

/// The questions running plugin calls are waiting on, oldest first.
#[tauri::command]
pub fn desktop_plugin_approvals_pending(
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let pending = approval::pending(&home(&state)?).map_err(failed)?;
    Ok(json!({
        "requests": pending
            .iter()
            .map(|request| json!({
                "id": request.id,
                "prompt": request.prompt(),
                "folder": request.folder,
                "path": request.path,
                "isDir": request.is_dir,
                "sessionId": request.session_id,
            }))
            .collect::<Vec<_>>()
    }))
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
