mod behavior;
mod inference;
mod principal;
mod profile;
mod skill;
mod task;
mod tools;

#[cfg(test)]
use defra_node::EmbeddedNode;
use gents::collection::Collection;
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, ConfigAccess, ConfigClientError,
    DesiredStateApplyPlan, DesiredStateError, InferenceProfileError, TransactionFailure,
};
#[cfg(test)]
use std::sync::Arc;

#[derive(Debug, thiserror::Error)]
pub enum ManageMutationError {
    #[error("Configuration must be an object.")]
    ConfigurationNotObject,
    #[error("Choose a model.")]
    ModelRequired,
    #[error("The selected agent no longer exists.")]
    SelectedAgentMissing,
    #[error("The selected agent has no inference profile.")]
    SelectedAgentHasNoProfile,
    #[error("The agent's inference profile is unavailable.")]
    AgentProfileUnavailable,
    #[error("default agent requires an existing node")]
    DefaultAgentRequiresNode,
    #[error("Agent {agent_id:?} does not exist")]
    AgentDoesNotExist { agent_id: String },
    #[error("component apply requires an existing node")]
    ComponentApplyRequiresNode,
    #[error("component apply requires scope-only node; use node save for node settings")]
    ComponentApplyRequiresScopeOnlyNode,
    #[error("component apply does not compile graph inputs; use the graph installer")]
    ComponentApplyGraphInputs,
    #[error("component owner does not match the selected node")]
    ComponentOwnerMismatch,
    #[error("component patch requires an owner")]
    ComponentPatchRequiresOwner,
    #[error("component patch requires a document ID")]
    ComponentPatchRequiresDocumentId,
    #[error("duplicate component patch identity")]
    DuplicateComponentPatchIdentity,
    #[error("component patch requires an existing node")]
    ComponentPatchRequiresNode,
    #[error("component patch requires existing {collection} {id}")]
    ComponentPatchRequiresDocument {
        collection: &'static str,
        id: String,
    },
    #[error("canonical config document must be an object")]
    CanonicalDocumentNotObject,
    #[error("This configuration no longer exists. Reopen Runtime Settings.")]
    ConfigurationMissing,
    #[error("Configuration has no unambiguous composite revision.")]
    ConfigurationRevisionAmbiguous,
    #[error("Configuration changed since Accounts opened. Reopen Accounts before changing authentication.")]
    AuthenticationRevisionChanged,
    #[error("Configuration exceeds 512 KiB.")]
    ConfigurationTooLarge,
    #[error("Enter a JSON object for configuration.")]
    ConfigurationNotJson(#[source] serde_json::Error),
    #[error("Only editable configuration fields are allowed. Authentication belongs in Accounts.")]
    ConfigurationFieldNotEditable,
    #[error("Configuration changed since this editor opened. Reopen Runtime Settings and apply your edits to the current version.")]
    EditorRevisionChanged,
    #[error("Saved configuration disappeared in its transaction.")]
    SavedConfigurationDisappeared,
    #[error(transparent)]
    Patch(#[from] gents::config_client::patch::PatchError),
    #[error(transparent)]
    Graphql(#[from] gents::graphql::GraphqlResponseError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    DesiredState(#[from] DesiredStateError),
    #[error(transparent)]
    InferenceProfile(Box<InferenceProfileError>),
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for ManageMutationError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::DesiredState(error) => error.config_client_error(),
            Self::InferenceProfile(error) => error.config_client_error(),
            Self::Graphql(error) => error.config_client_error(),
            _ => None,
        }
    }
}

type Result<T, E = ManageMutationError> = std::result::Result<T, E>;

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
    set_default_agent_on, upsert_node_on, ConfigEditorDocument, ConfigEditorRevision,
};
pub use profile::{
    delete_inference_profile_on, select_agent_model_on, upsert_inference_profile_on,
};
pub use skill::{delete_skill_on, upsert_skill_on};
pub use task::{
    delete_event_source_on, delete_schedule_on, delete_task_on, delete_trigger_on,
    fire_schedule_now, fire_task_now, prepare_access_review_task_on, resolve_schedule_now_on,
    resolve_task_now_on, upsert_event_source_on, upsert_schedule_on, upsert_task_on,
    upsert_trigger_on, ManualTaskInvocation, TaskMutationError,
};
pub use tools::{
    delete_tool_service_registry_on, delete_tools_on, upsert_tool_service_registry_on,
    upsert_tools_on,
};

impl From<InferenceProfileError> for ManageMutationError {
    fn from(error: InferenceProfileError) -> Self {
        Self::InferenceProfile(Box::new(error))
    }
}
