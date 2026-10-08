//! UX plugins from the desktop: the two runtime doors the webview loader
//! reads (`<home>/ux-plugins/` and installed packs' `ux[]`), one module
//! at a time, and a UX plugin's call into its own pack's `.afb` plugins.
//! All through `gents_server::packs`, against the managed local agent's
//! home; the desktop owns loading, gating and enable/disable.

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
            "ux plugins load from a local agent's home; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

/// Runs a pack operation on a blocking thread, as `packs.rs` does: a
/// producer run opens the WASI runtime, which must not sit on an async
/// task's thread.
async fn run<F, Fut, T>(operation: F) -> Result<T, BridgeError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
    T: Send + 'static,
{
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(operation()))
        .await
        .map_err(|error| BridgeError::untyped(format!("the ux plugin operation stopped: {error}")))?
        .map_err(failed)
}

#[tauri::command]
pub fn desktop_ux_plugins_list(state: State<'_, DesktopAppState>) -> Result<Value, BridgeError> {
    let plugins = packs::ux_list(&home(&state)?).map_err(failed)?;
    Ok(serde_json::json!({ "plugins": plugins }))
}

#[tauri::command]
pub async fn desktop_ux_plugin_source(
    id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    let module = run(move || packs::ux_module(home, id)).await?;
    serde_json::to_value(module).map_err(|error| BridgeError::untyped(error.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginCallRequest {
    /// The pack the calling ux plugin ships in, as `namespace/name`.
    pub pack: String,
    /// One of that pack's `.afb` plugins.
    pub name: String,
    #[serde(default)]
    pub input: Value,
}

#[tauri::command]
pub async fn desktop_plugin_call(
    request: PluginCallRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    let output =
        run(move || packs::plugin_call(home, request.pack, request.name, request.input)).await?;
    Ok(serde_json::json!({ "output": output }))
}
