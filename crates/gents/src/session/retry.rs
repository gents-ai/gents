use anyhow::Result;
use defra_node::EmbeddedNode;

pub(super) use crate::graphql::graphql_response_with_transaction_retry as execute_query_timed;

/// A session is active exactly while `closed_at` is absent; no parallel stored
/// status field participates in session lifecycle.
pub async fn count_active_sessions(node: &EmbeddedNode) -> Result<usize> {
    let query = r#"{
        AgentSession(filter: { closed_at: { _eq: null } }) { _docID }
    }"#;
    let response =
        crate::graphql::graphql_with_transaction_retry(node, query, "count active sessions")
            .await?;
    Ok(crate::graphql::rows::<serde_json::Value>(&response, "AgentSession")?.len())
}
