mod behavior;
mod inference;
mod principal;
mod profile;
mod skill;
mod task;
mod tools;

use anyhow::Result;
use gents::config_client::{apply_desired_state_plan, ConfigAccess, DesiredStateApplyPlan};

pub async fn apply_plan(
    access: &ConfigAccess,
    operation: &'static str,
    plan: DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact(operation, |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                Ok(())
            })
        })
        .await
}

pub use behavior::{delete_agent_behavior, upsert_agent_behavior_on};
pub use inference::{delete_inference_backend, upsert_inference_backend_on};
pub use principal::{
    apply_config_components, patch_config_components_on, upsert_agent_principal_on,
};
pub use profile::{delete_inference_profile, upsert_inference_profile_on};
pub use skill::{delete_skill, upsert_skill};
pub use task::{
    delete_event_source, delete_schedule, delete_task, delete_trigger, fire_schedule_now,
    fire_task_now, upsert_event_source, upsert_schedule, upsert_task, upsert_trigger,
};
pub use tools::{
    delete_tool_service_registry, delete_tools, upsert_tool_service_registry, upsert_tools,
};
