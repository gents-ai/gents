use super::*;

pub async fn load_node_scoped_snapshot(node: &EmbeddedNode, node_did: &str) -> Result<ClientStore> {
    let query = snapshot_query(Some(node_did))?;
    let data = execute_local_graphql_query(node, &query, "node-scoped desktop snapshot").await?;
    let requests = complete_local_snapshot_requests(node, &data, Some(node_did)).await?;
    snapshot_rows_with_requests(&data, Some(node_did), requests)
}
