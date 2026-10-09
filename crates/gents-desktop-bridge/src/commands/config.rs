use anyhow::Result;
use gents_desktop_core::client::ClientCore;

use super::super::types::{
    AgentDeleteRequest, AgentSaveRequest, BackendDeleteRequest, BackendSaveRequest,
    ContextDeleteRequest, DefaultAgentSetRequest, InferenceProfileDeleteRequest,
    InferenceProfileSaveRequest, NodeConfigSaveRequest, ScheduleDeleteRequest, SkillDeleteRequest,
    SkillSaveRequest, TaskDeleteRequest, ToolServiceDeleteRequest, ToolsDeleteRequest,
    ToolsSaveRequest, TriggerDeleteRequest,
};

pub async fn save_node_config(core: &ClientCore, request: NodeConfigSaveRequest) -> Result<()> {
    core.save_node(&request.document).await
}

pub async fn set_default_agent(core: &ClientCore, request: DefaultAgentSetRequest) -> Result<()> {
    core.set_default_agent(&request.node_did, &request.agent_id)
        .await
}

pub async fn save_agent_config(core: &ClientCore, request: AgentSaveRequest) -> Result<()> {
    core.save_agent(&request.document).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn save_skill_config(core: &ClientCore, request: SkillSaveRequest) -> Result<()> {
    core.save_skill(&request.document).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_skill_config(core: &ClientCore, request: SkillDeleteRequest) -> Result<()> {
    core.delete_skill(&request.skill_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_task_config(core: &ClientCore, request: TaskDeleteRequest) -> Result<()> {
    core.delete_task(&request.task_id, &request.node_did).await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_schedule_config(
    core: &ClientCore,
    request: ScheduleDeleteRequest,
) -> Result<()> {
    core.delete_schedule(&request.schedule_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_trigger_config(core: &ClientCore, request: TriggerDeleteRequest) -> Result<()> {
    core.delete_trigger(&request.trigger_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_backend_config(core: &ClientCore, request: BackendDeleteRequest) -> Result<()> {
    core.delete_inference_backend(&request.backend_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_inference_profile_config(
    core: &ClientCore,
    request: InferenceProfileDeleteRequest,
) -> Result<()> {
    core.delete_inference_profile(&request.profile_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_tools_config(core: &ClientCore, request: ToolsDeleteRequest) -> Result<()> {
    core.delete_tools(&request.tools_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_tool_service_config(
    core: &ClientCore,
    request: ToolServiceDeleteRequest,
) -> Result<()> {
    core.delete_tool_service(&request.service_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_agent_config(core: &ClientCore, request: AgentDeleteRequest) -> Result<()> {
    core.delete_agent(&request.agent_id, &request.node_did)
        .await
}

#[cfg_attr(test, allow(dead_code))]
pub async fn delete_context_config(core: &ClientCore, request: ContextDeleteRequest) -> Result<()> {
    core.delete_context(&request.context_id, &request.node_did)
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
    core.patch_config_components(&request.node_did, &patches)
        .await
}
