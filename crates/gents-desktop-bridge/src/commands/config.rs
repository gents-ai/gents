use anyhow::Result;
use gents_desktop_core::client::ClientCore;

use super::super::types::{
    AgentConfigSaveRequest, BackendDeleteRequest, BackendSaveRequest, BehaviorDeleteRequest,
    BehaviorSaveRequest, EventTriggerDeleteRequest, InferenceProfileDeleteRequest,
    InferenceProfileSaveRequest, ScheduleDeleteRequest, SkillDeleteRequest, SkillSaveRequest,
    TaskDeleteRequest, ToolServiceDeleteRequest, ToolsDeleteRequest, ToolsSaveRequest,
};

pub async fn save_agent_config(core: &ClientCore, request: AgentConfigSaveRequest) -> Result<()> {
    core.save_agent_principal(&request.document).await
}

pub async fn save_behavior_config(core: &ClientCore, request: BehaviorSaveRequest) -> Result<()> {
    core.save_behavior(&request.document).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn save_skill_config(core: &ClientCore, request: SkillSaveRequest) -> Result<()> {
    core.save_skill(&request.document).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_skill_config(core: &ClientCore, request: SkillDeleteRequest) -> Result<()> {
    core.delete_skill(&request.skill_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_task_config(core: &ClientCore, request: TaskDeleteRequest) -> Result<()> {
    core.delete_task(&request.task_id, &request.agent_did).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_schedule_config(
    core: &ClientCore,
    request: ScheduleDeleteRequest,
) -> Result<()> {
    core.delete_schedule(&request.schedule_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_event_trigger_config(
    core: &ClientCore,
    request: EventTriggerDeleteRequest,
) -> Result<()> {
    core.delete_event_trigger(&request.trigger_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_backend_config(core: &ClientCore, request: BackendDeleteRequest) -> Result<()> {
    core.delete_inference_backend(&request.backend_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_inference_profile_config(
    core: &ClientCore,
    request: InferenceProfileDeleteRequest,
) -> Result<()> {
    core.delete_inference_profile(&request.profile_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_tools_config(core: &ClientCore, request: ToolsDeleteRequest) -> Result<()> {
    core.delete_tools(&request.tools_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_tool_service_config(
    core: &ClientCore,
    request: ToolServiceDeleteRequest,
) -> Result<()> {
    core.delete_tool_service(&request.service_id, &request.agent_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_behavior_config(
    core: &ClientCore,
    request: BehaviorDeleteRequest,
) -> Result<()> {
    core.delete_behavior(&request.behavior_id, &request.agent_did)
        .await
}

pub async fn save_backend_config(core: &ClientCore, request: BackendSaveRequest) -> Result<()> {
    core.save_backend(&request.document).await
}

pub async fn save_inference_profile_config(
    core: &ClientCore,
    request: InferenceProfileSaveRequest,
) -> Result<()> {
    core.save_inference_profile(&request.document).await
}

pub async fn save_tools_config(core: &ClientCore, request: ToolsSaveRequest) -> Result<()> {
    core.save_tools(&request.document).await
}

pub async fn apply_config_components(
    core: &ClientCore,
    request: super::super::types::ConfigComponentsApplyRequest,
) -> Result<()> {
    core.apply_config_components(&request.document).await
}

pub async fn patch_config_components(
    core: &ClientCore,
    request: super::super::types::ConfigComponentsPatchRequest,
) -> Result<()> {
    let patches = request
        .patches
        .into_iter()
        .map(super::super::types::ConfigComponentPatch::into_patch)
        .collect::<Vec<_>>();
    core.patch_config_components(&request.agent_did, &patches)
        .await
}
