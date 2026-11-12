mod behavior;
mod inference;
mod principal;
mod profile;
mod skill;
mod task;
mod tools;

use anyhow::Result;
use gents_documents::apply_desired_state_plan;
use gents_documents::operations::delete;
use gents_documents::DesiredStateApplyPlan;
use gents_protocol::operations::document::{DocumentDeleteError, OperationActor};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

pub async fn apply_plan(
    access: &ConfigAccess,
    operation: &'static str,
    plan: DesiredStateApplyPlan,
) -> Result<()> {
    access
        .transact_write(operation, |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await?;
                Ok(())
            })
        })
        .await
}

/// Deletes one document of `node_did` as the User. `Ok(0)` is a target that
/// does not exist.
pub async fn delete_scoped_document(
    access: &ConfigAccess,
    operation: &'static str,
    collection: Collection,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    match delete::delete(
        access,
        operation,
        node_did,
        &[(collection, id.to_owned())],
        OperationActor::User,
        None,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await
    {
        Ok(receipt) => Ok(receipt.targets.len()),
        Err(DocumentDeleteError::TargetMissing { .. }) => Ok(0),
        Err(DocumentDeleteError::Passthrough(error)) => Err(anyhow::anyhow!(error)),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
pub(crate) use behavior::upsert_agent_on;
pub(crate) use behavior::upsert_agent_with_publication_on;
pub use behavior::{delete_agent_context_on, delete_agent_on};
pub use inference::{delete_inference_backend_on, upsert_inference_backend_on};
pub(crate) use principal::apply_config_components_with_publication_on;
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
    prepare_access_review_task_on, upsert_event_source_on, upsert_schedule_on, upsert_task_on,
    upsert_trigger_on,
};
pub use tools::{
    delete_tool_service_registry_on, delete_tools_on, upsert_tool_service_registry_on,
    upsert_tools_on,
};
