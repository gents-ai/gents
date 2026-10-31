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
#[cfg(test)]
use std::sync::Arc;

pub async fn apply_plan(
    access: &ConfigAccess,
    operation: &'static str,
    plan: DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact_write(operation, |txn| {
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
    node: &Arc<EmbeddedNode>,
    operation: &'static str,
    plan: DesiredStateApplyPlan,
) -> Result<()> {
    ConfigAccess::transact_local_write(node, None, operation, |txn| {
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
    node_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        collection,
        node_did.to_owned(),
        id.to_owned(),
    )])?;
    access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move {
                let existed = read_desired_state_record_in_txn(txn, collection, node_did, id)
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
    node: &Arc<EmbeddedNode>,
    operation: &'static str,
    collection: Collection,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    let plan = DesiredStateApplyPlan::new(Vec::new())?.with_removals(vec![(
        collection,
        node_did.to_owned(),
        id.to_owned(),
    )])?;
    ConfigAccess::transact_local_write(node, None, operation, |txn| {
        let plan = &plan;
        Box::pin(async move {
            let existed = read_desired_state_record_in_txn(txn, collection, node_did, id)
                .await?
                .is_some();
            apply_desired_state_plan(txn, plan).await?;
            Ok(usize::from(existed))
        })
    })
    .await
}

pub use behavior::{delete_agent_context_on, delete_agent_on, upsert_agent_on};
pub use inference::{delete_inference_backend_on, upsert_inference_backend_on};
pub use principal::{
    apply_config_components_on, patch_config_components_on, read_backend_authentication_on,
    read_config_editor_on, save_backend_authentication_on, save_config_editor_on,
    set_backend_rotation_on, set_default_agent_on, upsert_node_on, ConfigEditorDocument,
    ConfigEditorRevision,
};
pub use profile::{
    delete_inference_profile_on, select_agent_model_on, upsert_inference_profile_on,
};
pub use skill::{delete_skill_on, upsert_skill_on};
pub use task::{
    delete_event_source_on, delete_schedule_on, delete_task_on, delete_trigger_on,
    fire_schedule_now, fire_task_now, prepare_access_review_task_on, resolve_schedule_now_on,
    resolve_task_now_on, upsert_event_source_on, upsert_schedule_on, upsert_task_on,
    upsert_trigger_on, ManualTaskInvocation,
};
pub use tools::{
    delete_tool_service_registry_on, delete_tools_on, upsert_tool_service_registry_on,
    upsert_tools_on,
};
