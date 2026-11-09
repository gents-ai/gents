use crate::client::store::ClientStore;
use anyhow::Result;
use gents::store::config_client::ConfigAccess;

use super::super::graphql::normalize_required;

pub async fn rename_session(
    access: &ConfigAccess,
    store: &ClientStore,
    node_did: &str,
    requester_did: &str,
    session_id: &str,
    title: &str,
) -> Result<()> {
    let node_did = normalize_required("node_did", node_did)?;
    let requester_did = normalize_required("requester_did", requester_did)?;
    let session_id = normalize_required("session_id", session_id)?;
    let title = normalize_required("title", title)?;
    store
        .sessions
        .iter()
        .find(|row| {
            row.session_id == session_id
                && row.node_did == node_did
                && row.requester_did.as_deref() == Some(requester_did)
        })
        .ok_or_else(|| anyhow::anyhow!("session {} not found", session_id))?;

    gents::session::rename_session_on(access, node_did, requester_did, session_id, title).await
}
