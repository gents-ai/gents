use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use crate::store::graphql::graphql_with_transaction_retry;

/// The physical identity scopes live invalidations only; authority remains in
/// the native scoped read, and feed overflow still requires a durable reread.
pub(crate) struct ScopedInterruptObservation {
    pub(crate) request_doc_id: Option<String>,
    pub(crate) requested_at: Option<String>,
}

pub async fn fetch_interrupt_requested_at_scoped(
    node: &EmbeddedNode,
    request_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<Option<String>> {
    Ok(
        observe_interrupt_requested_at_scoped(node, request_id, node_did, requester_did)
            .await?
            .requested_at,
    )
}

pub(crate) async fn observe_interrupt_requested_at_scoped(
    node: &EmbeddedNode,
    request_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
) -> Result<ScopedInterruptObservation> {
    let filter = super::scoped_request_filter(request_id, node_did, requester_did)?;
    let query = format!(
        r#"query {{
            AgentRequest(
                filter: {{{filter}}},
                limit: 2
            ) {{
                _docID
                interrupt_requested_at
            }}
        }}"#
    );
    let response =
        graphql_with_transaction_retry(node, &query, "fetch_interrupt_requested_at_scoped")
            .await
            .with_context(|| format!("fetch_interrupt_requested_at_scoped({request_id})"))?;
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentRequest"))
        .and_then(serde_json::Value::as_array)
        .context("interrupt request fetch omitted rows")?;
    anyhow::ensure!(
        rows.len() <= 1,
        "interrupt request fetch is ambiguous within node scope"
    );
    let request_doc_id = rows
        .first()
        .map(|row| {
            row.get("_docID")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
                .context("scoped interrupt observation omitted physical request identity")
        })
        .transpose()?;
    let requested_at = rows
        .first()
        .and_then(|row| row.get("interrupt_requested_at"))
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    Ok(ScopedInterruptObservation {
        request_doc_id,
        requested_at,
    })
}
