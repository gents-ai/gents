//! Skill operations as every front end calls them (Lean
//! `DocumentOperations`), with the inference owner's profile policy bound.

use gents_inference::resolved::ProviderProfilePolicy;
use gents_protocol::operations::document::{
    DocumentDeleteReceipt, DocumentReceipt, OperationActor,
};
use gents_protocol::operations::skill::SkillOperationError;
use gents_store::config_client::ConfigAccess;

pub use gents_documents::operations::skill::{
    document_patch, get, list, load_source, resolve_owner, SkillImport, SkillSourceRoot, SkillWrite,
};

pub async fn save(
    access: &ConfigAccess,
    write: &SkillWrite<'_>,
) -> Result<DocumentReceipt, SkillOperationError> {
    gents_documents::operations::skill::save(access, write, &ProviderProfilePolicy, true)
        .await
        .map_err(gents_documents::operations::skill::finish)
}

pub async fn preview_save(
    access: &ConfigAccess,
    write: &SkillWrite<'_>,
) -> Result<DocumentReceipt, SkillOperationError> {
    gents_documents::operations::skill::save(access, write, &ProviderProfilePolicy, false)
        .await
        .map_err(gents_documents::operations::skill::finish)
}

pub async fn import(
    access: &ConfigAccess,
    import: &SkillImport<'_>,
) -> Result<DocumentReceipt, SkillOperationError> {
    gents_documents::operations::skill::import(access, import, &ProviderProfilePolicy, true)
        .await
        .map_err(gents_documents::operations::skill::finish)
}

pub async fn preview_import(
    access: &ConfigAccess,
    import: &SkillImport<'_>,
) -> Result<DocumentReceipt, SkillOperationError> {
    gents_documents::operations::skill::import(access, import, &ProviderProfilePolicy, false)
        .await
        .map_err(gents_documents::operations::skill::finish)
}

/// A person's delete: one call, scoped to `owner`.
pub async fn delete(
    access: &ConfigAccess,
    owner: &str,
    skill_id: &str,
) -> Result<DocumentDeleteReceipt, SkillOperationError> {
    gents_documents::operations::skill::delete(
        access,
        OperationActor::User,
        owner,
        skill_id,
        None,
        &ProviderProfilePolicy,
    )
    .await
    .map_err(gents_documents::operations::skill::finish)
}
