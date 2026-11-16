use anyhow::Result;
use gents_documents::ToolServiceRegistry;
use gents_documents::Tools;
use gents_store::config_client::ConfigAccess;

pub async fn upsert_tools_on(access: &ConfigAccess, document: &Tools) -> Result<()> {
    let input = gents_protocol::operations::tools::ToolsSaveInput {
        tools_id: document.tools_id.clone(),
        mode: gents_protocol::operations::document::WriteMode::Save,
        patch: gents::operations::tools::document_patch(document)?,
        allow_drop: gents_documents::patch::SelfConfigTarget::Tools
            .writable_fields()
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        shared_acknowledged: true,
    };
    gents::operations::tools::save(access, &document.node_did, &input, None).await?;
    Ok(())
}

pub async fn upsert_tool_service_registry_on(
    access: &ConfigAccess,
    document: &ToolServiceRegistry,
) -> Result<()> {
    let patch = gents::operations::mcp_service::document_patch(document)?;
    gents::operations::mcp_service::save(
        access,
        &gents::operations::mcp_service::McpServiceWrite {
            owner: &document.node_did,
            service_id: &document.service_id,
            mode: gents_protocol::operations::document::WriteMode::Save,
            patch: &patch,
            node_binding: None,
        },
    )
    .await?;
    Ok(())
}

pub async fn delete_tools_on(access: &ConfigAccess, node_did: &str, id: &str) -> Result<usize> {
    let receipt = gents::operations::tools::delete(access, node_did, id).await?;
    Ok(receipt.targets.len())
}

pub async fn delete_tool_service_registry_on(
    access: &ConfigAccess,
    node_did: &str,
    id: &str,
) -> Result<usize> {
    let receipt = gents::operations::mcp_service::delete(access, node_did, id).await?;
    Ok(receipt.targets.len())
}
