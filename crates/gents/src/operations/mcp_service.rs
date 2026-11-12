//! MCP service operations as every front end calls them (Lean
//! `DocumentOperations`), with the inference owner's profile policy bound.

use gents_inference::resolved::ProviderProfilePolicy;
use gents_protocol::operations::document::{DeleteReceipt, DocumentReceipt, OperationActor};
use gents_protocol::operations::mcp_service::{McpRegisterInput, McpServiceOperationError};
use gents_store::config_client::ConfigAccess;

pub use gents_documents::operations::mcp_service::{document_patch, McpServiceWrite};

pub async fn register(
    access: &ConfigAccess,
    owner: &str,
    input: &McpRegisterInput,
) -> Result<DocumentReceipt, McpServiceOperationError> {
    gents_documents::operations::mcp_service::register(
        access,
        owner,
        input,
        None,
        &ProviderProfilePolicy,
        true,
    )
    .await
    .map_err(gents_documents::operations::mcp_service::finish)
}

pub async fn save(
    access: &ConfigAccess,
    write: &McpServiceWrite<'_>,
) -> Result<DocumentReceipt, McpServiceOperationError> {
    gents_documents::operations::mcp_service::save(access, write, &ProviderProfilePolicy, true)
        .await
        .map_err(gents_documents::operations::mcp_service::finish)
}

/// A person's delete: one call, scoped to `owner`.
pub async fn delete(
    access: &ConfigAccess,
    owner: &str,
    service_id: &str,
) -> Result<DeleteReceipt, McpServiceOperationError> {
    gents_documents::operations::mcp_service::delete(
        access,
        OperationActor::User,
        owner,
        service_id,
        None,
        &ProviderProfilePolicy,
        true,
    )
    .await
    .map_err(gents_documents::operations::mcp_service::finish)
}
