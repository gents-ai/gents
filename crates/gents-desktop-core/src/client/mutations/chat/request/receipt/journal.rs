use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents::config_client::ConfigAccess;
use gents::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use std::sync::Arc;

use super::UnresolvedSubmission;

/// The local encrypted store retains signed intent before the owner can see it.
/// This collection stays outside the desktop replication catalog.
pub(crate) async fn save(node: &Arc<EmbeddedNode>, pending: &UnresolvedSubmission) -> Result<()> {
    let request_id = escape_graphql_string(&pending.request_id);
    let payload = escape_graphql_string(&serde_json::to_string(pending)?);
    ConfigAccess::write_local(
        node,
        "desktop.chat.retain_submission",
        &format!("mutation {{ create_PendingChatSubmission(input: {{request_id: \"{request_id}\", payload: \"{payload}\"}}) {{ _docID }} }}"),
    )
    .await?;
    Ok(())
}

pub(crate) async fn load(
    node: &EmbeddedNode,
    request_id: &str,
) -> Result<Option<UnresolvedSubmission>> {
    let escaped = escape_graphql_string(request_id);
    let response = graphql_with_transaction_retry(
        node,
        &format!("{{ PendingChatSubmission(filter: {{request_id: {{_eq: \"{escaped}\"}}}}, limit: 2) {{payload}} }}"),
        "desktop.chat.load_submission",
    )
    .await?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("PendingChatSubmission"))
        .and_then(serde_json::Value::as_array)
        .context("pending submission query omitted rows")?;
    anyhow::ensure!(rows.len() <= 1, "retained submission identity is ambiguous");
    rows.first()
        .map(|row| {
            let payload = row
                .get("payload")
                .and_then(serde_json::Value::as_str)
                .context("pending submission has no payload")?;
            let pending: UnresolvedSubmission =
                serde_json::from_str(payload).context("decoding retained submission")?;
            pending.validate_binding(request_id)?;
            Ok(pending)
        })
        .transpose()
}

pub(crate) async fn forget(node: &Arc<EmbeddedNode>, request_id: &str) {
    let id = escape_graphql_string(request_id);
    if let Err(error) = ConfigAccess::write_local(
        node,
        "desktop.chat.forget_submission",
        &format!("mutation {{ delete_PendingChatSubmission(filter: {{request_id: {{_eq: \"{id}\"}}}}) {{ _docID }} }}"),
    ).await {
        tracing::warn!(%request_id, %error, "retained submission cleanup failed after its outcome was resolved");
    }
}
