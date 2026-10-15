use super::*;

/// Load the desktop snapshot projection for one node from the embedded replica.
pub async fn load_node_scoped_snapshot(node: &EmbeddedNode, node_did: &str) -> Result<ClientStore> {
    let did = escape_graphql_string(node_did);
    let filter = format!("filter: {{ node_did: {{ _eq: \"{did}\" }} }}");
    let query = snapshot_query(Some(&filter))?;
    let data = execute_local_graphql_query(node, &query, "node-scoped desktop snapshot").await?;
    snapshot_rows(&data, Some(node_did))
}
