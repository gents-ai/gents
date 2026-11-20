use super::bootstrap::normalize_required;
use super::ClientRuntime;
use anyhow::{Context, Result};
use gents_protocol::file_changes::ToolFileChanges;
use gents_store::config_client::ConfigAccess;

mod inventory;

impl ClientRuntime {
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
        gents_observe::client_file_review::tool_file_changes_on(
            &access,
            node_did,
            session_id,
            request_id,
            tool_call_doc_id,
        )
        .await
    }
}
