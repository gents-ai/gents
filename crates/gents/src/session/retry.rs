use anyhow::Result;
use defra_node::EmbeddedNode;

pub(super) use crate::graphql::graphql_response_with_transaction_retry as execute_query_timed;

pub async fn count_active_sessions(node: &EmbeddedNode) -> Result<usize> {
    let query = r#"{
        AgentSession(filter: { status: { _eq: "active" } }) { _docID }
    }"#;
    let response =
        crate::graphql::graphql_with_transaction_retry(node, query, "count active sessions")
            .await?;
    Ok(crate::graphql::rows::<serde_json::Value>(&response, "AgentSession")?.len())
}
