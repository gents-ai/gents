//! UX plugins from the desktop: the two runtime doors the webview loader
//! reads (`<home>/ux-plugins/` and installed packs' `ux[]`), one module
//! at a time, a UX plugin's call into its own pack's `.afb` plugins, and
//! its calls into the datastore tools its manifest declares. All through
//! `gents_server::packs`, against the managed local agent's home; the
//! desktop owns loading, gating and enable/disable.

use gents_protocol::error_chain::ErrorChain;
use serde::Deserialize;
use serde_json::Value;
use tauri::State;
use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::{current_core, DesktopAppState};
use gents_server::packs;

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "ux plugins load from a local agent's home; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(ErrorChain(error.as_ref()).to_string())
}

/// The operator's access to the agent `node_did`'s node: the principal a
/// page's data call reads and writes as.
fn operator_access(
    state: &DesktopAppState,
    node_did: &str,
) -> Result<gents::ConfigAccess, BridgeError> {
    let core = current_core(state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientNotRunning,
            "the desktop client is not started; a ux plugin's data call needs the agent's node",
        )
    })?;
    core.operator_access(node_did).map_err(|error| {
        BridgeError::new(
            BridgeErrorCode::EndpointUnreachable,
            format!(
                "resolving the operator's access to node {node_did}: {}",
                ErrorChain(error.as_ref())
            ),
        )
    })
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
    gents::storage_backend::drive_unsendable(tauri::async_runtime::handle().inner(), operation)
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

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PluginCallRequest {
    /// The runtime listing of the calling ux plugin (`<ns>/<pack>/<name>`);
    /// the pack is resolved from it here, never named by the webview.
    pub ux: String,
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
        run(move || packs::plugin_call(home, request.ux, request.name, request.input)).await?;
    Ok(serde_json::json!({ "output": output }))
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UxDataCallRequest {
    /// The runtime listing of the calling ux plugin (`<ns>/<pack>/<name>`);
    /// its manifest's `data` is what the call is checked against.
    pub ux: String,
    /// The agent whose `DatastoreToolSurface` is called.
    pub node_did: String,
    pub surface: String,
    pub tool: String,
    /// The tool's own arguments.
    #[serde(default)]
    pub args: Value,
}

/// One declared datastore tool, run as the operator on the agent's node;
/// answers `{"kind": "query" | "create", "output": <the tool's result>}`.
#[tauri::command]
pub async fn desktop_ux_data_call(
    request: UxDataCallRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    let access = operator_access(&state, &request.node_did)?;
    packs::ux_data_call(
        home,
        access,
        gents::ux_data::UxDataCall {
            ux: request.ux,
            node_did: request.node_did,
            surface: request.surface,
            tool: request.tool,
            args: request.args,
        },
    )
    .await
    .map_err(failed)
}

#[derive(Debug, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UxDataSurfacesRequest {
    pub ux: String,
    pub node_did: String,
}

/// The surfaces and tools the ux plugin may call on the agent, as the
/// node has them: `{"surfaces": [...]}`.
#[tauri::command]
pub async fn desktop_ux_data_surfaces(
    request: UxDataSurfacesRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    let home = home(&state)?;
    let access = operator_access(&state, &request.node_did)?;
    packs::ux_data_surfaces(home, access, request.ux, request.node_did)
        .await
        .map_err(failed)
}
