use tauri::{AppHandle, Emitter, State};

use super::super::commands::{
    run_schedule_config, run_task_config, save_event_trigger_config, save_schedule_config,
    save_task_config,
};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    ClientUpdateEvent, DesktopClientSnapshot, EventTriggerSaveRequest, ScheduleRunRequest,
    ScheduleSaveRequest, TaskRunRequest, TaskRunResult, TaskSaveRequest,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub(crate) async fn desktop_task_save(
    app: AppHandle,
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
pub(crate) async fn desktop_schedule_save(
    app: AppHandle,
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
pub(crate) async fn desktop_schedule_run(
    app: AppHandle,
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
pub(crate) async fn desktop_event_trigger_save(
    app: AppHandle,
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
pub(crate) async fn desktop_task_run(
    app: AppHandle,
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
