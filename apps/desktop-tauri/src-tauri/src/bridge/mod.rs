mod snapshot;
mod state;
mod types;

use std::sync::Arc;

use defra_agent_desktop::client::{ClientCore, DesktopPaths};
use defra_agent_desktop::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, init_standard_local_runtime,
    reset_desktop_runtime_state, DesktopInitOptions, DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, State};

use self::snapshot::{
    build_bootstrap_summary, build_client_snapshot, build_session_snapshot_from_store,
};
use self::state::{current_core, spawn_client_update_task, DesktopAppState};
use self::types::{
    ChatSendRequest, ChatSendResult, ClientUpdateEvent, DesktopBootstrapSummary,
    DesktopClientSnapshot, DesktopInitRequest, DesktopSessionSnapshot,
};

#[tauri::command]
fn desktop_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    tauri::async_runtime::block_on(build_bootstrap_summary())
}

#[tauri::command]
fn desktop_init_local_standard(
    request: DesktopInitRequest,
) -> Result<DesktopInitSummary, String> {
    tauri::async_runtime::block_on(async move {
        let agent_home = match request.agent_home {
            Some(path) => path,
            None => default_agent_home().map_err(|error| error.to_string())?,
        };
        let desktop_paths = match request.desktop_home {
            Some(root) => DesktopPaths::from_root(root),
            None => DesktopPaths::discover().map_err(|error| error.to_string())?,
        };

        if request.dangerously_overwrite {
            dangerously_overwrite_desktop_home(desktop_paths.root())
                .map_err(|error| error.to_string())?;
        } else if request.reset {
            let _ =
                reset_desktop_runtime_state(&desktop_paths).map_err(|error| error.to_string())?;
        }

        init_standard_local_runtime(DesktopInitOptions {
            agent_home,
            desktop_paths,
            label: request
                .label
                .filter(|label| !label.trim().is_empty())
                .unwrap_or_else(|| "Local Agent".to_string()),
        })
        .await
        .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_client_start(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    if let Some(core) = current_core(&state) {
        return tauri::async_runtime::block_on(build_client_snapshot(Some(&core)));
    }

    let core = Arc::new(
        tauri::async_runtime::block_on(ClientCore::start())
            .map_err(|error| error.to_string())?,
    );
    let updates_task = spawn_client_update_task(app.clone(), Arc::clone(&core));

    {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        bridge.core = Some(Arc::clone(&core));
        bridge.updates_task = Some(updates_task);
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    tauri::async_runtime::block_on(build_client_snapshot(Some(&core)))
}

#[tauri::command]
fn desktop_client_shutdown(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let (core, updates_task) = {
        let mut bridge = state.bridge.lock().expect("desktop bridge lock poisoned");
        (bridge.core.take(), bridge.updates_task.take())
    };

    if let Some(task) = updates_task {
        task.abort();
    }

    if let Some(core) = core {
        tauri::async_runtime::block_on(core.shutdown()).map_err(|error| error.to_string())?;
    }

    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent {
            reason: "lifecycle",
        },
    );

    tauri::async_runtime::block_on(build_client_snapshot(None))
}

#[tauri::command]
fn desktop_client_snapshot(
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let core = current_core(&state);
    tauri::async_runtime::block_on(build_client_snapshot(core.as_ref()))
}

#[tauri::command]
fn desktop_session_snapshot(
    session_id: String,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, String> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };

    let snapshot = tauri::async_runtime::block_on(async move { core.store().snapshot() });
    Ok(build_session_snapshot_from_store(snapshot.as_ref(), &session_id))
}

#[tauri::command]
fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        return Err("agent_did is required".to_string());
    }

    let content = request.content.trim().to_string();
    if content.is_empty() {
        return Err("content is required".to_string());
    }

    let behavior_id = request
        .behavior_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    tauri::async_runtime::block_on(async move {
        let session_id = match request
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(session_id) => session_id.to_string(),
            None => core
                .create_conversation(&agent_did, behavior_id.as_deref())
                .await
                .map_err(|error| error.to_string())?
                .session_id,
        };

        let submitted = core
            .submit_request(&session_id, &agent_did, &content, behavior_id.as_deref())
            .await
            .map_err(|error| error.to_string())?;

        Ok(ChatSendResult {
            session_id,
            request_id: submitted.request_id,
            agent_did: submitted.agent_did,
            behavior_id: submitted.behavior_id,
        })
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(DesktopAppState::default())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap_summary,
            desktop_init_local_standard,
            desktop_client_start,
            desktop_client_shutdown,
            desktop_client_snapshot,
            desktop_session_snapshot,
            desktop_chat_send
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
