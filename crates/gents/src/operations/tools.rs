//! Tools operations as every front end calls them (Lean
//! `SelfConfig.ToolsSave`). The actor is whoever `access` signs for: a person
//! here, since an Agent saves through its config tool.

use gents_inference::resolved::ProviderProfilePolicy;
use gents_protocol::operations::document::{DocumentDeleteReceipt, OperationActor};
use gents_protocol::operations::tools::{ToolsOperationError, ToolsSaveInput};
use gents_store::config_client::ConfigAccess;
use gents_tools::builtin::self_config::tools_operation::{finish, save_tools, ToolsActor};
use gents_tools::builtin::self_config::OperatorGrants;

pub use gents_tools::builtin::self_config::tools_operation::{
    document_patch, InstalledPlugins, ToolsReceipt,
};

async fn run(
    access: &ConfigAccess,
    owner: &str,
    input: &ToolsSaveInput,
    installed: Option<InstalledPlugins<'_>>,
    commit: bool,
) -> Result<ToolsReceipt, ToolsOperationError> {
    let holds_catalog = gents_tools::builtin::self_config::holds_agent_catalog(access, owner)
        .await
        .map_err(|error| {
            gents_protocol::operations::document::DocumentOperationError::Passthrough(
                error.reallocate_into_boxed_dyn_error_without_backtrace(),
            )
        })?;
    let actor = ToolsActor {
        actor: OperationActor::User,
        agent_held: OperatorGrants::default(),
        holds_catalog,
        installed,
    };
    save_tools(access, owner, input, &actor, commit)
        .await
        .map_err(finish)
}

/// Saves one Tools document of `owner` as the person `access` signs for.
pub async fn save(
    access: &ConfigAccess,
    owner: &str,
    input: &ToolsSaveInput,
    installed: Option<InstalledPlugins<'_>>,
) -> Result<ToolsReceipt, ToolsOperationError> {
    run(access, owner, input, installed, true).await
}

pub async fn preview_save(
    access: &ConfigAccess,
    owner: &str,
    input: &ToolsSaveInput,
    installed: Option<InstalledPlugins<'_>>,
) -> Result<ToolsReceipt, ToolsOperationError> {
    run(access, owner, input, installed, false).await
}

/// A person's delete: one call, scoped to `owner`.
pub async fn delete(
    access: &ConfigAccess,
    owner: &str,
    tools_id: &str,
) -> Result<DocumentDeleteReceipt, ToolsOperationError> {
    let targets = [(
        gents_documents::patch::SelfConfigTarget::Tools.collection(),
        tools_id.to_owned(),
    )];
    gents_documents::operations::delete::delete(
        access,
        "operations.tools.delete",
        owner,
        &targets,
        OperationActor::User,
        None,
        &ProviderProfilePolicy,
    )
    .await
    .map_err(ToolsOperationError::Delete)
}
