use serde::Deserialize;
use serde_json::Value;
use tauri::State;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::state::DesktopAppState;
use gents_server::packs::{self, EditedDocuments};

fn home(state: &DesktopAppState) -> Result<std::path::PathBuf, BridgeError> {
    state.policy.node_home.clone().ok_or_else(|| {
        BridgeError::new(
            BridgeErrorCode::Unsupported,
            "packs install into a local Node; start one first",
        )
    })
}

fn failed(error: anyhow::Error) -> BridgeError {
    BridgeError::untyped(format!("{error:#}"))
}

async fn run<F, Fut>(operation: F) -> Result<Value, BridgeError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = anyhow::Result<Value>>,
{
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || handle.block_on(operation()))
        .await
        .map_err(|error| BridgeError::untyped(format!("the pack operation stopped: {error}")))?
        .map_err(failed)
}

#[tauri::command]
pub async fn desktop_pack_installed(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::installed(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_search(
    query: String,
    page: Option<u32>,
    registry: Option<String>,
) -> Result<Value, BridgeError> {
    {
        run(move || packs::search(registry, query, page.unwrap_or(1).max(1))).await
    }
}

#[tauri::command]
pub async fn desktop_pack_info(
    package: String,
    registry: Option<String>,
) -> Result<Value, BridgeError> {
    {
        run(move || packs::info(registry, package)).await
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackInstallRequest {
    pub package: String,
    #[serde(default)]
    pub registry: Option<String>,
    #[serde(default)]
    pub inference_slots: Vec<String>,
    #[serde(default)]
    pub edited: EditedDocuments,
    #[serde(default)]
    pub grant_authority: bool,
    #[serde(default)]
    pub preview: bool,
}

#[tauri::command]
pub async fn desktop_pack_install(
    request: PackInstallRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || {
            packs::install(
                home,
                request.package,
                request.registry,
                request.inference_slots,
                request.edited,
                request.grant_authority,
                request.preview,
            )
        })
        .await
    }
}

#[tauri::command]
pub async fn desktop_pack_update(
    package: Option<String>,
    registry: Option<String>,
    edited: Option<EditedDocuments>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::update(home, package, registry, edited.unwrap_or_default())).await
    }
}

#[tauri::command]
pub async fn desktop_pack_remove(
    package: String,
    edited: Option<EditedDocuments>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::remove(home, package, edited.unwrap_or_default())).await
    }
}

#[tauri::command]
pub async fn desktop_pack_login(
    token: String,
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::login(home, registry, token)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_logout(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::logout(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_whoami(
    registry: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::whoami(home, registry)).await
    }
}

#[tauri::command]
pub async fn desktop_pack_plugin_slots(
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::plugin_slots(home)).await
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginBindRequest {
    pub plugin: String,
    #[serde(default)]
    pub profile: Option<String>,
}

#[tauri::command]
pub async fn desktop_pack_plugin_bind(
    request: PluginBindRequest,
    state: State<'_, DesktopAppState>,
) -> Result<Value, BridgeError> {
    {
        let home = home(&state)?;
        run(move || packs::bind_plugin_slot(home, request.plugin, request.profile)).await
    }
}
