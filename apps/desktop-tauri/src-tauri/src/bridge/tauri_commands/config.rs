use tauri::{AppHandle, State};

use super::super::commands::{
    delete_event_trigger_config, delete_schedule_config, delete_skill_config, delete_task_config,
    save_agent_config, save_backend_config, save_behavior_config, save_inference_profile_config,
    save_skill_config, save_tool_selection_config, save_tool_service_config,
    test_tool_service_config,
};
use super::super::state::{current_core, DesktopAppState};
use super::super::types::{
    AgentConfigSaveRequest, BackendSaveRequest, BehaviorSaveRequest, DesktopClientSnapshot,
    EventTriggerDeleteRequest, InferenceProfileSaveRequest, ScheduleDeleteRequest,
    SkillDeleteRequest, SkillSaveRequest, TaskDeleteRequest, ToolSelectionSaveRequest,
    ToolServiceSaveRequest, ToolServiceTestRequest, ToolServiceTestResult,
};
use super::emit_config_update_and_snapshot;

#[tauri::command]
pub(crate) fn desktop_agent_config_save(
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
pub(crate) fn desktop_behavior_save(
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
pub(crate) fn desktop_skill_save(
    app: AppHandle,
    request: SkillSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        save_skill_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_skill_delete(
    app: AppHandle,
    request: SkillDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        delete_skill_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_backend_save(
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
pub(crate) fn desktop_inference_profile_save(
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
pub(crate) fn desktop_tool_selection_save(
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
pub(crate) fn desktop_tool_service_save(
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
pub(crate) fn desktop_tool_service_test(
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
pub(crate) fn desktop_task_delete(
    app: AppHandle,
    request: TaskDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        delete_task_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_schedule_delete(
    app: AppHandle,
    request: ScheduleDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        delete_schedule_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}

#[tauri::command]
pub(crate) fn desktop_event_trigger_delete(
    app: AppHandle,
    request: EventTriggerDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, String> {
    let Some(core) = current_core(&state) else {
        return Err("desktop client is not running".to_string());
    };

    tauri::async_runtime::block_on(async move {
        delete_event_trigger_config(core.as_ref(), request)
            .await
            .map_err(|error| error.to_string())?;
        emit_config_update_and_snapshot(&app, &core).await
    })
}
