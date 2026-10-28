use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents_protocol::row::AgentRequestRow;

use crate::error::{BridgeError, BridgeErrorCode};

use super::super::writes::find_request_in_scope;

/// Image selection needs the current physical admission, not publication of a
/// transcript projection. The image owner still verifies accepted-tool binding
/// and source authorization; a missing replica cannot establish missing bytes.
pub(super) async fn load_request_scope(
    access: &ConfigAccess,
    node_did: &str,
    session_id: &str,
    request_id: &str,
) -> Result<AgentRequestRow> {
    access
        .transact_readonly("file_review.request_scope", |txn| {
            Box::pin(async move {
                let session = gents::session::load_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                )
                .await?
                .context(BridgeError::new(
                    BridgeErrorCode::ProjectionPending,
                    "This chat's file-review scope is not available yet. Open its changes again after it syncs.",
                ))?;
                let scope = gents::session::public_request_filter(
                    &gents::session::session_scope_filter(
                        node_did,
                        session_id,
                        session.session.requester_did.as_deref(),
                    ),
                );
                let response = txn
                    .execute(&format!(
                        r#"{{ AgentRequest(filter: {{ {scope}, request_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID request_id node_did session_id requester_did }} }}"#,
                        escape_graphql_string(request_id),
                    ))
                    .await?;
                let rows = response["data"]["AgentRequest"]
                    .as_array()
                    .context("file-review scope lookup omitted request rows")?
                    .iter()
                    .cloned()
                    .map(serde_json::from_value)
                    .collect::<std::result::Result<Vec<AgentRequestRow>, _>>()?;
                let row = find_request_in_scope(&rows, request_id, Some(node_did))?
                    .context(BridgeError::new(
                        BridgeErrorCode::ProjectionPending,
                        "This request's file-review scope is not available yet. Open its changes again after it syncs.",
                    ))?;
                anyhow::ensure!(
                    row.session_id.as_deref() == Some(session_id)
                        && row.requester_did == session.session.requester_did,
                    "file changes crossed the selected request scope"
                );
                anyhow::ensure!(
                    row.doc_id.as_deref().is_some_and(|id| !id.is_empty()),
                    "request lacks physical identity"
                );
                Ok(row)
            })
        })
        .await
}
