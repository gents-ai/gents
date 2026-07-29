use tauri::{AppHandle, Runtime, State};

use crate::error::BridgeError;

use super::emit_config_update_and_snapshot;
use crate::commands::{
    delete_backend_config, delete_behavior_config, delete_event_trigger_config,
    delete_inference_profile_config, delete_schedule_config, delete_skill_config,
    delete_task_config, delete_tool_selection_config, delete_tool_service_config,
    save_agent_config, save_backend_config, save_behavior_config, save_inference_profile_config,
    save_skill_config, save_tool_selection_config, save_tool_service_config,
    test_tool_service_config,
};
use crate::state::{current_core, DesktopAppState};
use crate::types::{
    AgentConfigSaveRequest, BackendDeleteRequest, BackendSaveRequest, BehaviorDeleteRequest,
    BehaviorSaveRequest, DesktopClientSnapshot, EventTriggerDeleteRequest,
    InferenceProfileDeleteRequest, InferenceProfileSaveRequest, ScheduleDeleteRequest,
    SkillDeleteRequest, SkillSaveRequest, TaskDeleteRequest, ToolSelectionDeleteRequest,
    ToolSelectionSaveRequest, ToolServiceDeleteRequest, ToolServiceSaveRequest,
    ToolServiceTestRequest, ToolServiceTestResult,
};

#[tauri::command]
pub async fn desktop_agent_config_save<R: Runtime>(
    app: AppHandle<R>,
    request: AgentConfigSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_agent_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_behavior_save<R: Runtime>(
    app: AppHandle<R>,
    request: BehaviorSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_behavior_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_skill_save<R: Runtime>(
    app: AppHandle<R>,
    request: SkillSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_skill_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_skill_delete<R: Runtime>(
    app: AppHandle<R>,
    request: SkillDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_skill_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_backend_save<R: Runtime>(
    app: AppHandle<R>,
    request: BackendSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_backend_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_inference_profile_save<R: Runtime>(
    app: AppHandle<R>,
    request: InferenceProfileSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_inference_profile_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_tool_selection_save<R: Runtime>(
    app: AppHandle<R>,
    request: ToolSelectionSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_tool_selection_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_tool_service_save<R: Runtime>(
    app: AppHandle<R>,
    request: ToolServiceSaveRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    save_tool_service_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_tool_service_test(
    request: ToolServiceTestRequest,
    state: State<'_, DesktopAppState>,
) -> Result<ToolServiceTestResult, BridgeError> {
    let _ = current_core(&state)
        .ok_or_else(|| BridgeError::from_legacy_message("desktop client is not running"))?;

    test_tool_service_config(request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))
}

#[tauri::command]
pub async fn desktop_task_delete<R: Runtime>(
    app: AppHandle<R>,
    request: TaskDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_task_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_schedule_delete<R: Runtime>(
    app: AppHandle<R>,
    request: ScheduleDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_schedule_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_event_trigger_delete<R: Runtime>(
    app: AppHandle<R>,
    request: EventTriggerDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_event_trigger_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_backend_delete<R: Runtime>(
    app: AppHandle<R>,
    request: BackendDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_backend_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_inference_profile_delete<R: Runtime>(
    app: AppHandle<R>,
    request: InferenceProfileDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_inference_profile_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_tool_selection_delete<R: Runtime>(
    app: AppHandle<R>,
    request: ToolSelectionDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_tool_selection_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_tool_service_delete<R: Runtime>(
    app: AppHandle<R>,
    request: ToolServiceDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_tool_service_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}

#[tauri::command]
pub async fn desktop_behavior_delete<R: Runtime>(
    app: AppHandle<R>,
    request: BehaviorDeleteRequest,
    state: State<'_, DesktopAppState>,
) -> Result<DesktopClientSnapshot, BridgeError> {
    let Some(core) = current_core(&state) else {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    };

    delete_behavior_config(core.as_ref(), request)
        .await
        .map_err(|error| BridgeError::from_legacy_message(error.to_string()))?;
    emit_config_update_and_snapshot(&app, &core, &state).await
}
