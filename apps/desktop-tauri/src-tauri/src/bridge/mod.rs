mod commands;
mod logging;
mod snapshot;
mod state;
#[cfg(test)]
mod tests;
mod types;

use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

use defra_agent_desktop_core::client::{ClientCore, DesktopPaths};
use defra_agent_desktop_core::local_runtime::{
    dangerously_overwrite_desktop_home, default_agent_home, fetch_runtime_connection_payload,
    init_standard_local_runtime, reset_desktop_runtime_state, DesktopInitOptions,
    DesktopInitSummary,
};
use tauri::{AppHandle, Emitter, State};

use self::commands::{
    add_peer, rename_conversation, repair_p2p, run_schedule_config, run_task_config,
    save_agent_config, save_backend_config, save_behavior_config, save_event_trigger_config,
    save_inference_profile_config, save_schedule_config, save_task_config,
    save_tool_selection_config, save_tool_service_config, send_chat_message,
    test_tool_service_config,
};
use self::logging::init_tracing;
use self::snapshot::{
    build_bootstrap_summary, build_client_snapshot, build_session_snapshot_from_store_for_agent,
};
use self::state::{current_core, spawn_client_update_task, DesktopAppState};
use self::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, ChatSendRequest,
    ChatSendResult, ClientUpdateEvent, ConversationRenameRequest, DesktopBootstrapSummary,
    DesktopClientSnapshot, DesktopInitRequest, DesktopSessionSnapshot, EventTriggerSaveRequest,
    InferenceProfileSaveRequest, PeerAddRequest, PeerStatusFetchRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest, ToolSelectionSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult,
};

static TAURI_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

async fn emit_config_update_and_snapshot(
    app: &AppHandle,
    core: &Arc<ClientCore>,
) -> Result<DesktopClientSnapshot, String> {
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    build_client_snapshot(Some(core)).await
}

#[tauri::command]
fn desktop_bootstrap_summary() -> Result<DesktopBootstrapSummary, String> {
    tauri::async_runtime::block_on(build_bootstrap_summary())
}

#[tauri::command]
fn desktop_init_local_standard(request: DesktopInitRequest) -> Result<DesktopInitSummary, String> {
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
        tauri::async_runtime::block_on(ClientCore::start()).map_err(|error| error.to_string())?,
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
fn desktop_peer_add(
    app: AppHandle,
    request: PeerAddRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        add_peer(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_peer_status_fetch(request: PeerStatusFetchRequest) -> Result<serde_json::Value, String> {
    tauri::async_runtime::block_on(async move {
        fetch_runtime_connection_payload(&request.server_address)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_p2p_repair(
    app: AppHandle,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        repair_p2p(core.as_ref(), Duration::from_millis(250))
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
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
    agent_did: Option<String>,
    request_id: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<Option<DesktopSessionSnapshot>, String> {
    let Some(core) = current_core(&state) else {
        return Ok(None);
    };

    let snapshot = tauri::async_runtime::block_on(async move { core.store().snapshot() });
    Ok(build_session_snapshot_from_store_for_agent(
        snapshot.as_ref(),
        agent_did.as_deref(),
        &session_id,
        request_id.as_deref(),
    ))
}

#[tauri::command]
fn desktop_chat_send(
    request: ChatSendRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ChatSendResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        send_chat_message(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_conversation_rename(
    request: ConversationRenameRequest,
    state: State<'_, DesktopAppState>,
) -> Result<(), String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        rename_conversation(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_agent_config_save(
    app: AppHandle,
    request: AgentConfigSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_agent_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_behavior_save(
    app: AppHandle,
    request: BehaviorSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_behavior_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_backend_save(
    app: AppHandle,
    request: BackendSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_backend_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_inference_profile_save(
    app: AppHandle,
    request: InferenceProfileSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_inference_profile_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_selection_save(
    app: AppHandle,
    request: ToolSelectionSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_tool_selection_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_service_save(
    app: AppHandle,
    request: ToolServiceSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_tool_service_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_tool_service_test(
    request: ToolServiceTestRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ToolServiceTestResult, String> {
    let _ = current_core(&state).ok_or_else(|| "desktop client is not running".to_string())?;

    tauri::async_runtime::block_on(async move {
        test_tool_service_config(request)
            .await
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn desktop_task_save(
    app: AppHandle,
    request: TaskSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_task_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_schedule_save(
    app: AppHandle,
    request: ScheduleSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_schedule_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_schedule_run(
    app: AppHandle,
    request: ScheduleRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let result = run_schedule_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        let _ = app.emit(
            "desktop://client-updated",
            ClientUpdateEvent { reason: "config" },
        );
        Ok(result)
    })
}

#[tauri::command]
fn desktop_event_trigger_save(
    app: AppHandle,
    request: EventTriggerSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_event_trigger_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
fn desktop_task_run(
    app: AppHandle,
    request: TaskRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        let result = run_task_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        let _ = app.emit(
            "desktop://client-updated",
            ClientUpdateEvent { reason: "config" },
        );
        Ok(result)
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    let runtime = TAURI_RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            // Merge replay can recurse deeply through collection heads. Give Tokio
            // worker threads enough stack to reach the merge depth guard instead of
            // aborting the whole desktop process with a native stack overflow.
            .thread_stack_size(32 * 1024 * 1024)
            .build()
            .expect("failed to build Tauri Tokio runtime")
    });
    tauri::async_runtime::set(runtime.handle().clone());

    tauri::Builder::default()
        .manage(DesktopAppState::default())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            desktop_bootstrap_summary,
            desktop_init_local_standard,
            desktop_client_start,
            desktop_client_shutdown,
            desktop_peer_add,
            desktop_peer_status_fetch,
            desktop_p2p_repair,
            desktop_client_snapshot,
            desktop_session_snapshot,
            desktop_chat_send,
            desktop_conversation_rename,
            desktop_agent_config_save,
            desktop_behavior_save,
            desktop_backend_save,
            desktop_inference_profile_save,
            desktop_tool_selection_save,
            desktop_tool_service_save,
            desktop_tool_service_test,
            desktop_task_save,
            desktop_schedule_save,
            desktop_schedule_run,
            desktop_event_trigger_save,
            desktop_task_run
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
