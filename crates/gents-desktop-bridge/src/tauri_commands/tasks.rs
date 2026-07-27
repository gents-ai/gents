use tauri::{Runtime, AppHandle, Emitter, State};

use crate::commands::{
    run_schedule_config, run_task_config, save_event_trigger_config, save_schedule_config,
    save_task_config,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    ClientUpdateEvent, DesktopClientSnapshot, EventTriggerSaveRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub async fn desktop_task_save<R: Runtime>(
    app: AppHandle<R>,
    request: TaskSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_task_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub async fn desktop_schedule_save<R: Runtime>(
    app: AppHandle<R>,
    request: ScheduleSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub async fn desktop_schedule_run<R: Runtime>(
    app: AppHandle<R>,
    request: ScheduleRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let result = run_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    Ok(result)
}

#[tauri::command]
pub async fn desktop_event_trigger_save<R: Runtime>(
    app: AppHandle<R>,
    request: EventTriggerSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_event_trigger_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub async fn desktop_task_run<R: Runtime>(
    app: AppHandle<R>,
    request: TaskRunRequest,
    state: State<'_, DesktopAppState>,
) -> Result<TaskRunResult, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    let result = run_task_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    let _ = app.emit(
        "desktop://client-updated",
        ClientUpdateEvent { reason: "config" },
    );
    Ok(result)
}
