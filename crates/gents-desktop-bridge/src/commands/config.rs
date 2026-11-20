use crate::client_runtime::ClientRuntime;
use anyhow::Result;

use super::super::types::{
    AgentDeleteRequest, AgentSaveRequest, BackendDeleteRequest, BackendRotationSetRequest,
    BackendSaveRequest, ContextDeleteRequest, DefaultAgentSetRequest,
    InferenceProfileDeleteRequest, InferenceProfileSaveRequest, NodeConfigSaveRequest,
    ScheduleDeleteRequest, SkillDeleteRequest, SkillSaveRequest, TaskDeleteRequest,
    ToolServiceDeleteRequest, ToolsDeleteRequest, ToolsSaveRequest, TriggerDeleteRequest,
};

pub async fn save_node_config(core: &ClientRuntime, request: NodeConfigSaveRequest) -> Result<()> {
    core.save_node(&request.document).await
}

pub async fn set_default_agent(
    core: &ClientRuntime,
    request: DefaultAgentSetRequest,
) -> Result<()> {
    core.set_default_agent(&request.node_did, &request.agent_did)
        .await
}

pub async fn save_agent_config(core: &ClientRuntime, request: AgentSaveRequest) -> Result<()> {
    core.save_agent(&request.document, request.context_patch.as_ref())
        .await
}

pub async fn save_skill_config(core: &ClientRuntime, request: SkillSaveRequest) -> Result<()> {
    core.save_skill(&request.document).await
}

pub async fn delete_skill_config(core: &ClientRuntime, request: SkillDeleteRequest) -> Result<()> {
    core.delete_skill(&request.skill_id, &request.node_did)
        .await
}

pub async fn delete_task_config(core: &ClientRuntime, request: TaskDeleteRequest) -> Result<()> {
    core.delete_task(&request.task_id, &request.node_did).await
}

pub async fn delete_schedule_config(
    core: &ClientRuntime,
    request: ScheduleDeleteRequest,
) -> Result<()> {
    core.delete_schedule(&request.schedule_id, &request.node_did)
        .await
}

pub async fn delete_trigger_config(
    core: &ClientRuntime,
    request: TriggerDeleteRequest,
) -> Result<()> {
    core.delete_trigger(&request.trigger_id, &request.node_did)
        .await
}

pub async fn delete_backend_config(
    core: &ClientRuntime,
    request: BackendDeleteRequest,
) -> Result<()> {
    core.delete_inference_backend(&request.backend_id, &request.node_did)
        .await
}

pub async fn delete_inference_profile_config(
    core: &ClientRuntime,
    request: InferenceProfileDeleteRequest,
) -> Result<()> {
    core.delete_inference_profile(&request.profile_id, &request.node_did)
        .await
}

pub async fn delete_tools_config(core: &ClientRuntime, request: ToolsDeleteRequest) -> Result<()> {
    core.delete_tools(&request.tools_id, &request.node_did)
        .await
}

pub async fn delete_tool_service_config(
    core: &ClientRuntime,
    request: ToolServiceDeleteRequest,
) -> Result<()> {
    core.delete_tool_service(&request.service_id, &request.node_did)
        .await
}

pub async fn delete_agent_config(core: &ClientRuntime, request: AgentDeleteRequest) -> Result<()> {
    core.delete_agent(&request.agent_did, &request.node_did)
        .await
}

pub async fn delete_context_config(
    core: &ClientRuntime,
    request: ContextDeleteRequest,
) -> Result<()> {
    core.delete_context(&request.context_id, &request.node_did)
        .await
}

pub async fn save_backend_config(core: &ClientRuntime, request: BackendSaveRequest) -> Result<()> {
    core.save_backend(&request.document).await
}

pub async fn set_backend_rotation_config(
    core: &ClientRuntime,
    request: BackendRotationSetRequest,
) -> Result<()> {
    core.set_backend_rotation(
        &request.node_did,
        &request.backend_id,
        None,
        request.rotation,
    )
    .await
    .map(drop)
}

pub async fn save_inference_profile_config(
    core: &ClientRuntime,
    request: InferenceProfileSaveRequest,
) -> Result<()> {
    core.save_inference_profile(&request.document).await
}

pub async fn save_tools_config(core: &ClientRuntime, request: ToolsSaveRequest) -> Result<()> {
    core.save_tools(&request.document).await
}

pub async fn apply_config_components(
    core: &ClientRuntime,
    request: super::super::types::ConfigComponentsApplyRequest,
) -> Result<()> {
    core.apply_config_components(&request.document).await
}

pub async fn patch_config_components(
    core: &ClientRuntime,
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
