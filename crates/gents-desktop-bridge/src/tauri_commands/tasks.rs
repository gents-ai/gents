use tauri::{AppHandle, Emitter, Runtime, State};

use crate::error::BridgeError;

use super::emit_config_update_and_snapshot;
use crate::commands::{
    delete_event_source_config, run_schedule_config, run_task_config, save_event_source_config,
    save_schedule_config, save_task_config, save_trigger_config,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ClientUpdateEvent, DesktopClientSnapshot, EventSourceDeleteRequest, EventSourceSaveRequest,
    ScheduleRunRequest, ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest,
    TriggerSaveRequest,
};

#[tauri::command]
pub async fn desktop_task_save<R: Runtime>(
    app: AppHandle<R>,
    request: TaskSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    save_task_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_schedule_save<R: Runtime>(
    app: AppHandle<R>,
    request: ScheduleSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    save_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_schedule_run<R: Runtime>(
    app: AppHandle<R>,
    request: ScheduleRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let result = run_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    Ok(result)
}

#[tauri::command]
pub async fn desktop_trigger_save<R: Runtime>(
    app: AppHandle<R>,
    request: TriggerSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    save_trigger_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_task_run<R: Runtime>(
    app: AppHandle<R>,
    request: TaskRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };

    let result = run_task_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent::coarse("config"),
    );
    Ok(result)
}

#[tauri::command]
pub async fn desktop_event_source_save<R: Runtime>(
    app: AppHandle<R>,
    request: EventSourceSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    save_event_source_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_event_source_delete<R: Runtime>(
    app: AppHandle<R>,
    request: EventSourceDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::untyped("desktop client is not running"));
    };
    delete_event_source_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::untyped(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}
