use anyhow::Result;

use crate::store::graphql::escape_graphql_string;

/// Read the runtime's published readiness and its native ownership in one snapshot.
/// A completed publication marker does not prove the parent grant still exists.
pub(crate) async fn observe_published_readiness(
    txn: &super::ConfigApplyTxn<'_>,
    runtime: &identity::Did,
) -> Result<Option<gents_protocol::row::NodeReadinessRow>> {
    anyhow::ensure!(
        txn.actor_did() == Some(runtime.as_str()),
        "readiness observation actor mismatch"
    );
    let did = escape_graphql_string(runtime.as_str());
    let response = txn.execute(&format!(
        r#"{{ Node(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID}}
        NodeReadiness(filter: {{node_did: {{_eq: "{did}"}}}}, limit: 2) {{_docID node_did node_doc_id parent_linked snapshot_json updated_at}} }}"#
    )).await?;
    let parents = gents_protocol::graphql::graphql_rows_from_response(&response, "Node");
    let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "NodeReadiness");
    let ([parent], [row]) = (parents.as_slice(), rows.as_slice()) else {
        return Ok(None);
    };
    let Some(parent_id) = parent["_docID"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let Some(doc_id) = row["_docID"].as_str().filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    if row["node_doc_id"].as_str() != Some(parent_id)
        || row["parent_linked"].as_bool() != Some(true)
    {
        return Ok(None);
    }
    let parents = txn.document_owners("Node", &[parent_id.to_owned()]).await?;
    let owners = txn
        .document_owners("NodeReadiness", &[doc_id.to_owned()])
        .await?;
    if parents.len() != 1
        || !parents[0].visible
        || parents[0].owner.is_none()
        || owners.len() != 1
        || !owners[0].visible
        || owners[0].owner.as_ref() != Some(runtime)
    {
        return Ok(None);
    }
    Ok(Some(serde_json::from_value(row.clone())?))
}
