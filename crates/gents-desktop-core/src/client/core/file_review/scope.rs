use anyhow::{Context, Result};
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session::SessionScope;
use gents_session::views::requests::RequestRows;
use gents_session::views::transcript::Scope;
use gents_store::config_client::ConfigAccess;

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
                let session = gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, node_did, session_id,
                )
                .await?
                .context(BridgeError::new(
                    BridgeErrorCode::ProjectionPending,
                    "This chat's file-review scope is not available yet. Open its changes again after it syncs.",
                ))?;
                let view = RequestRows::new(Scope::Session(SessionScope {
                    node_did: node_did.to_owned(),
                    session_id: session_id.to_owned(),
                    requester_did: session.session.requester_did.clone(),
                }))
                .public()
                .request_ids(vec![request_id.to_owned()])
                .limit(2);
                let rows = txn.read_view(&view).await?;
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
