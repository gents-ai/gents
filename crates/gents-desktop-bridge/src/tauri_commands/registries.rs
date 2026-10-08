//! The registry index from the desktop: what the Packs panel's registry
//! picker reads (`gents registry list`), refreshes from the master, and
//! edits by hand. Same operations as the CLI, through `gents_server::packs`.

use serde::Deserialize;
use serde_json::Value;
use tauri::State;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use gents_server::packs;

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.agent_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "the registry index lives in a local agent's home; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

#[tauri::command]
pub fn desktop_registry_list(state: State<'_, DesktopAppState>) -> Result<Value, BridgeError> {
    packs::registry_list(&home(&state)?).map_err(failed)
}

#[tauri::command]
pub async fn desktop_registry_refresh(
    master: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    packs::registry_refresh(home, master).await.map_err(failed)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryAddRequest {
    pub id: String,
    pub url: String,
    #[serde(default)]
    pub label: Option<String>,
}

#[tauri::command]
pub fn desktop_registry_add(
    request: RegistryAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    packs::registry_add(
        &home(&state)?,
        &request.id,
        &request.url,
        request.label.as_deref(),
    )
    .map_err(failed)
}

#[tauri::command]
pub fn desktop_registry_remove(
    id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    packs::registry_remove(&home(&state)?, &id).map_err(failed)
}
