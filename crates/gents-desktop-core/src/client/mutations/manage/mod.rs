mod behavior;
mod inference;
mod principal;
mod profile;
mod skill;
mod task;
mod tools;

use anyhow::Result;
#[cfg(test)]
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess, DesiredStateApplyPlan,
};

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

#[cfg(test)]
pub async fn apply_plan_local(
    node: &EmbeddedNode,
    operation: &'static str,
    plan: DesiredStateApplyPlan,
) -> Result<()> {
    ConfigAccess::transact_local(node, None, operation, |txn| {
        let plan = &plan;
        Box::pin(async move {
            apply_desired_state_plan(txn, plan).await?;
            Ok(())
        })
    })
    .await
}

pub async fn delete_scoped_document(
    access: &ConfigAccess,
    operation: &'static str,
    collection: Collection,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        collection,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    access
        .transact(operation, |txn| {
            let plan = &plan;
            Box::pin(async move {
                let existed = read_desired_state_record_in_txn(txn, collection, agent_did, id)
                    .await?
                    .is_some();
                apply_desired_state_plan(txn, plan).await?;
                Ok(usize::from(existed))
            })
        })
        .await
}

#[cfg(test)]
pub async fn delete_scoped_document_local(
    node: &EmbeddedNode,
    operation: &'static str,
    collection: Collection,
    agent_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        collection,
        agent_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local(node, None, operation, |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(txn, collection, agent_did, id)
                .await?
                .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub use behavior::{delete_agent_behavior_on, delete_agent_context_on, upsert_agent_behavior_on};
pub use inference::{delete_inference_backend_on, upsert_inference_backend_on};
pub use principal::{
    apply_config_components_on, patch_config_components_on, upsert_agent_principal_on,
};
pub use profile::{delete_inference_profile_on, upsert_inference_profile_on};
pub use skill::{delete_skill_on, upsert_skill_on};
pub use task::{
    delete_event_source_on, delete_schedule_on, delete_task_on, delete_trigger_on,
    fire_schedule_now, fire_task_now, resolve_schedule_now_on, resolve_task_now_on,
    upsert_event_source_on, upsert_schedule_on, upsert_task_on, upsert_trigger_on,
    ManualTaskInvocation,
};
pub use tools::{
    delete_tool_service_registry_on, delete_tools_on, upsert_tool_service_registry_on,
    upsert_tools_on,
};
