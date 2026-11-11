use anyhow::Result;

/// Pending terminal paths can encounter malformed request rows. Cleanup
/// requires the complete binding tuple from the winning update, but absent
/// provenance must not prevent rejection by physical document ID.
pub async fn release_terminal_writer_binding(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request: &serde_json::Value,
) -> Result<()> {
    let string = |field: &str| {
        request[field]
            .as_str()
            .filter(|value| !value.trim().is_empty())
    };
    let (Some(doc_id), Some(request_id), Some(workspace_id), Some(owner)) = (
        string("_docID"),
        string("request_id"),
        string("workspace_id"),
        string("workspace_owner_node_did"),
    ) else {
        return Ok(());
    };
    release_writer_binding_in_txn(txn, doc_id, request_id, workspace_id, owner).await
}

pub async fn release_writer_binding_in_txn(
    txn: &crate::store::config_client::ConfigApplyTxn<'_>,
    request_doc_id: &str,
    request_id: &str,
    workspace_id: &str,
    owner: &str,
) -> Result<()> {
    let release = {
        use gents_protocol::document_query::{
            fields::workspace_binding as fields, Filter, Input, Update,
        };
        Update::matching(
            Filter::eq(fields::WORKSPACE_ID, workspace_id)
                .and(Filter::eq(fields::OWNER_NODE_DID, owner))
                .and(Filter::eq(fields::REQUEST_ID, request_id))
                .and(Filter::eq(fields::REQUEST_DOC_ID, request_doc_id))
                .and(Filter::eq(fields::LIFECYCLE_STATE, "active")),
            Input::new().set(fields::LIFECYCLE_STATE, "released"),
        )
        .render()
    };
    txn.run(&release).await?;
    Ok(())
}
