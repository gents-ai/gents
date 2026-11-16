use super::bootstrap::normalize_required;
use super::ClientCore;
use crate::error::{BridgeError, BridgeErrorCode};
use anyhow::{Context, Result};
use gents_protocol::file_changes::ToolFileChanges;
use gents_session::tool_call::query::result::{
    load_tool_file_change_observation, ToolFileChangeRead,
};
use gents_store::config_client::ConfigAccess;

mod inventory;
mod scope;

impl ClientCore {
    pub async fn tool_file_changes(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
        tool_call_doc_id: &str,
        retry_id: Option<&str>,
    ) -> Result<Option<ToolFileChanges>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let request_id = normalize_required("request_id", request_id)?;
        let tool_call_doc_id = normalize_required("tool_call_doc_id", tool_call_doc_id)?;
        retry_id.map(uuid::Uuid::parse_str).transpose()?;
        let access = ConfigAccess::Graphql(
            self.operator_observation_graphql(node_did)
                .context("Connect to the runtime to read recorded file versions.")?,
        );
        tool_file_changes_on(&access, node_did, session_id, request_id, tool_call_doc_id).await
    }
}

/// The recorded file changes of `tool_call_doc_id` within `request_id`'s
/// admitted scope in `node_did`'s session, read over `access`.
pub async fn tool_file_changes_on(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    tool_call_doc_id: &str,
) -> Result<Option<ToolFileChanges>> {
    let row = scope::load_request_scope(access, node_did, session_id, request_id).await?;
    let request_doc_id = row
        .doc_id
        .as_deref()
        .context("request lacks physical identity")?;
    match load_tool_file_change_observation(
        access,
        tool_call_doc_id,
        request_doc_id,
        node_did,
        session_id,
        row.requester_did.as_deref(),
    )
    .await?
    {
        ToolFileChangeRead::Retained { changes, .. } => Ok(Some(changes)),
        ToolFileChangeRead::Missing => Ok(None),
        ToolFileChangeRead::Pending => Err(BridgeError::new(
            BridgeErrorCode::ProjectionPending,
            "This edit is still running. Open its changes again when it finishes.",
        )
        .into()),
        ToolFileChangeRead::Unsupported => Err(BridgeError::new(
            BridgeErrorCode::Unsupported,
            "This runtime does not support recorded file versions.",
        )
        .into()),
    }
}
