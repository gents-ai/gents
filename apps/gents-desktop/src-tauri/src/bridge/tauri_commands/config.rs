use tauri::{AppHandle, State};

use gents_desktop_bridge::commands::{
    delete_backend_config, delete_behavior_config, delete_event_trigger_config,
    delete_inference_profile_config, delete_schedule_config, delete_skill_config,
    delete_task_config, delete_tool_selection_config, delete_tool_service_config,
    save_agent_config, save_backend_config, save_behavior_config, save_inference_profile_config,
    save_skill_config, save_tool_selection_config, save_tool_service_config,
    test_tool_service_config,
};
use super::super::state::{current_core, DesktopAppState};
use gents_desktop_bridge::types::{
    AgentConfigSaveRequest, BackendDeleteRequest, BackendSaveRequest, BehaviorDeleteRequest,
    BehaviorSaveRequest, DesktopClientSnapshot, EventTriggerDeleteRequest,
    InferenceProfileDeleteRequest, InferenceProfileSaveRequest, ScheduleDeleteRequest,
    SkillDeleteRequest, SkillSaveRequest, TaskDeleteRequest, ToolSelectionDeleteRequest,
    ToolSelectionSaveRequest, ToolServiceDeleteRequest, ToolServiceSaveRequest,
    ToolServiceTestRequest, ToolServiceTestResult,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub(crate) async fn desktop_agent_config_save(
    app: AppHandle,
    request: AgentConfigSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_agent_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_behavior_save(
    app: AppHandle,
    request: BehaviorSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_behavior_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_skill_save(
    app: AppHandle,
    request: SkillSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_skill_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_skill_delete(
    app: AppHandle,
    request: SkillDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_skill_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_backend_save(
    app: AppHandle,
    request: BackendSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_backend_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_inference_profile_save(
    app: AppHandle,
    request: InferenceProfileSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_inference_profile_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_tool_selection_save(
    app: AppHandle,
    request: ToolSelectionSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_tool_selection_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_tool_service_save(
    app: AppHandle,
    request: ToolServiceSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    save_tool_service_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_tool_service_test(
    request: ToolServiceTestRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ToolServiceTestResult, String> {
    let _ = current_core(&state).ok_or_else(|| "desktop client is not running".to_string())?;

    test_tool_service_config(request)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub(crate) async fn desktop_task_delete(
    app: AppHandle,
    request: TaskDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_task_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_schedule_delete(
    app: AppHandle,
    request: ScheduleDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_event_trigger_delete(
    app: AppHandle,
    request: EventTriggerDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_event_trigger_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_backend_delete(
    app: AppHandle,
    request: BackendDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_backend_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_inference_profile_delete(
    app: AppHandle,
    request: InferenceProfileDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_inference_profile_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_tool_selection_delete(
    app: AppHandle,
    request: ToolSelectionDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_tool_selection_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_tool_service_delete(
    app: AppHandle,
    request: ToolServiceDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_tool_service_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}

#[tauri::command]
pub(crate) async fn desktop_behavior_delete(
    app: AppHandle,
    request: BehaviorDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    delete_behavior_config(core.as_ref(), request)
        .await
        .map_err(|error| error.to_string())?;
    emit_config_update_and_snapshot(&app, &core).await
}
