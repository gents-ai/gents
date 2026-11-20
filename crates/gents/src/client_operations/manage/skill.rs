use crate::operations::skill::SkillWrite;
use anyhow::Result;
use gents_documents::SkillDocument;
use gents_protocol::operations::document::WriteMode;
use gents_store::config_client::ConfigAccess;

pub async fn upsert_skill_on(access: &ConfigAccess, document: &SkillDocument) -> Result<()> {
    let patch = crate::operations::skill::document_patch(document)?;
    crate::operations::skill::save(
        access,
        &SkillWrite {
            owner: &document.node_did,
            skill_id: &document.skill_id,
            mode: WriteMode::Save,
            patch: &patch,
            node_binding: None,
        },
    )
    .await?;
    Ok(())
}

pub async fn delete_skill_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    let receipt = crate::operations::skill::delete(access, node_did, id).await?;
    Ok(receipt.targets.len())
}
