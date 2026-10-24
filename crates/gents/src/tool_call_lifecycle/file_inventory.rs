use crate::config_client::ConfigApplyTxn;
use crate::graphql::escape_graphql_string;
use crate::session_origin::SessionScope;
use crate::toolset::FileChanges;
use anyhow::{ensure, Context, Result};

/// The caller owns the winning terminal and the image observation in this same
/// transaction. Metadata keeps its own document identity; it grants no image
/// reader relation and is never included in ordinary replication catalogs.
pub(super) async fn retain_inventory(
    txn: &ConfigApplyTxn<'_>,
    observation_doc_id: &str,
    tool_call_doc_id: &str,
    request_doc_id: &str,
    scope: &SessionScope,
    changes: &FileChanges,
) -> Result<()> {
    ensure!(
        !observation_doc_id.is_empty(),
        "file inventory omitted its image observation"
    );
    let input = serde_json::json!({
        "tool_call_doc_id": tool_call_doc_id, "observation_doc_id": observation_doc_id,
        "request_doc_id": request_doc_id, "node_did": scope.node_did,
        "session_id": scope.session_id, "requester_did": scope.requester_did,
        "changes": changes.metadata(),
    });
    let existing = txn.execute(&format!(
        r#"{{ ToolFileInventory(filter: {{ tool_call_doc_id: {{ _eq: "{}" }} }}, limit: 2) {{
        _docID tool_call_doc_id observation_doc_id request_doc_id node_did session_id requester_did changes }} }}"#,
        escape_graphql_string(tool_call_doc_id),
    )).await?;
    let rows = existing["data"]["ToolFileInventory"]
        .as_array()
        .context("file inventory lookup omitted projections")?;
    if let [row] = rows.as_slice() {
        let mut observed = row.clone();
        let id = observed
            .as_object_mut()
            .context("file inventory projection is not an object")?
            .remove("_docID")
            .context("file inventory projection omitted its identity")?;
        let id = id
            .as_str()
            .context("file inventory projection identity is not a string")?
            .to_owned();
        ensure!(
            observed == input,
            "file inventory conflicts with an existing projection"
        );
        let owners = txn
            .document_owners("ToolFileInventory", std::slice::from_ref(&id))
            .await?;
        ensure!(
            owners.len() == 1
                && owners[0].doc_id == id
                && owners[0]
                    .owner
                    .as_ref()
                    .is_some_and(|owner| owner.as_str() == scope.node_did),
            "file inventory projection is not owned by its source node"
        );
        return Ok(());
    }
    ensure!(rows.is_empty(), "file inventory has multiple projections");
    let result = txn.execute_with_variables(
        "mutation($input: ToolFileInventoryMutationInputArg!) { create_ToolFileInventory(input: $input) { _docID } }",
        &serde_json::json!({"input": input}),
    ).await?;
    ensure!(
        crate::graphql::mutation_documents(&result, "create_ToolFileInventory")?
            .is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str().is_some()),
        "file inventory projection was not retained"
    );
    #[cfg(test)]
    {
        let mut failures = FAIL_NEXT.lock().unwrap();
        if let Some(index) = failures.iter().position(|id| id == tool_call_doc_id) {
            failures.remove(index);
            return Err(crate::config_client::classified_conflict_for_test());
        }
    }
    Ok(())
}

#[cfg(test)]
static FAIL_NEXT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(crate) fn fail_next_publication(tool_call_doc_id: &str) {
    FAIL_NEXT.lock().unwrap().push(tool_call_doc_id.to_owned());
}
