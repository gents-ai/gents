//! UX plugins from the desktop: the two runtime doors the webview loader
//! reads (`<home>/ux-plugins/` and installed packs' `ux[]`), one module
//! at a time, a UX plugin's call into its own pack's `.afb` plugins, and
//! its calls into the datastore tools its manifest declares. All through
//! `gents_server::packs`, against the managed local agent's home; the
//! desktop owns loading, gating and enable/disable.

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
    BridgeError::untyped(format!("{error:#}"))
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
            format!("resolving the operator's access to node {node_did}: {error:#}"),
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
    gents::store::local::storage_backend::drive_unsendable(
        tauri::async_runtime::handle().inner(),
        operation,
    )
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
    let operator =
        if packs::plugin_call_uses_data(&home, &request.ux, &request.name).map_err(failed)? {
            let node_did = packs::ux_installing_node(&home).map_err(failed)?;
            let core = current_core(&state).ok_or_else(|| {
                BridgeError::new(
                    BridgeErrorCode::ClientNotRunning,
                    "the local node that installed this app is not connected",
                )
            })?;
            let access = core
                .local_home_operator_access(&node_did, &home)
                .map_err(failed)?;
            Some((access, node_did))
        } else {
            None
        };
    let output =
        run(move || packs::plugin_call(home, request.ux, request.name, request.input, operator))
            .await?;
    Ok(serde_json::json!({ "output": output }))
}

#[derive(Debug, Clone, Deserialize, serde::Serialize, TS)]
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

#[derive(Debug, Deserialize, serde::Serialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum UxDataCallKind {
    Query,
    Create,
}

#[derive(Debug, Deserialize, serde::Serialize, TS)]
pub struct UxDataCallResult {
    pub kind: UxDataCallKind,
    pub output: Value,
}

#[derive(Debug, serde::Serialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UxDataWatchEvent {
    Snapshot { value: UxDataCallResult },
    Error { message: String },
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
        gents::observe::ux_data::UxDataCall {
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

/// The page's endpoint is acquired here once, so a node that has no managed
/// runtime fails the call, and again by the task at every connect.
#[tauri::command]
pub fn desktop_ux_data_watch<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    request: UxDataCallRequest,
    channel: tauri::ipc::Channel<UxDataWatchEvent>,
) -> Result<String, BridgeError>
where
    R: tauri::Runtime,
{
    let home = home(&state)?;
    let core = current_core(&state).ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::ClientNotRunning,
            "the desktop client is not started",
        )
    })?;
    let access = core
        .local_home_operator_access(&request.node_did, &home)
        .map_err(failed)?;
    if !matches!(access, gents::ConfigAccess::Graphql(_)) {
        return Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "pack observation needs a managed runtime",
        ));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let label = window.label().to_owned();
    let registry = state.ux_watches.clone();
    let cancel = {
        let bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        if !bridge.window_selects_node(&label, &request.node_did) {
            return Err(BridgeError::untyped("the selected node changed"));
        }
        registry.insert(&label, &id).map_err(failed)?
    };
    let app = tauri::Manager::app_handle(&window).clone();
    let task_id = id.clone();
    tauri::async_runtime::spawn(async move {
        let node_did = request.node_did.clone();
        let acquire = || reacquire(&app, &node_did, &home);
        let deliver = |event: UxDataWatchEvent| {
            registry.deliver(&label, &task_id, || channel.send(event).is_ok())
        };
        let observe = |phase: crate::ux_watch::WatchPhase,
                       event: crate::ux_watch::WatchEvent,
                       next: crate::ux_watch::WatchPhase| {
            tracing::debug!(?phase, ?event, ?next, "pack observation transition");
        };
        crate::ux_watch::run(
            crate::ux_watch::WATCH_TIMING,
            acquire,
            &request,
            deliver,
            &cancel,
            observe,
        )
        .await;
        registry.cancel(&label, &task_id);
    });
    Ok(id)
}

/// The User's current access to the node's managed runtime. A missing core
/// or route is the client restarting, so the attempt is retried; an endpoint
/// that is not a managed runtime cannot become one.
fn reacquire<R>(
    app: &tauri::AppHandle<R>,
    node_did: &str,
    home: &std::path::Path,
) -> Result<gents::store::config_client::GraphqlEndpoint, crate::ux_watch::WatchEvent>
where
    R: tauri::Runtime,
{
    use crate::ux_watch::WatchEvent;
    use tauri::Manager;
    let state = app.state::<DesktopAppState>();
    let core = current_core(&state).ok_or(WatchEvent::TransientError)?;
    match core.local_home_operator_access(node_did, home) {
        Ok(gents::ConfigAccess::Graphql(endpoint)) => Ok(endpoint),
        Ok(_) => Err(WatchEvent::PermanentError),
        Err(_) => Err(WatchEvent::TransientError),
    }
}

#[tauri::command]
pub fn desktop_ux_data_unwatch<R>(
    window: tauri::WebviewWindow<R>,
    state: State<'_, DesktopAppState>,
    id: String,
) where
    R: tauri::Runtime,
{
    state.ux_watches.cancel(window.label(), &id);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BootstrapPolicy, BridgeConfig, HomePolicy};
    use crate::ux_watch::WatchEvent;

    /// A watch whose client core is gone (the client is restarting) retries
    /// its next attempt instead of ending the page's observation.
    #[test]
    fn reacquiring_without_a_client_core_is_transient() {
        let temp = tempfile::tempdir().unwrap();
        let policy = crate::state::resolve_policy(
            &BridgeConfig {
                home: HomePolicy::FixedRoot(temp.path().join("desktop")),
                bootstrap: BootstrapPolicy::PairedRemoteOnly,
                store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
                ..Default::default()
            },
            None,
        )
        .unwrap();
        let app = tauri::test::mock_builder()
            .manage(DesktopAppState::new(policy))
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        let error = reacquire(app.handle(), "did:key:node", temp.path()).unwrap_err();
        assert_eq!(error, WatchEvent::TransientError);
    }
}
