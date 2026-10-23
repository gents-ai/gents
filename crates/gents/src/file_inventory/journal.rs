use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_inventory::{FileInventoryPageDemand, FileInventoryReply};

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::escape_graphql_string;
use crate::NodeIdentity;

fn require_source(node: &EmbeddedNode, demand: &FileInventoryPageDemand) -> Result<()> {
    demand.validate_reference()?;
    ensure!(
        node.node_identity_did() == Some(demand.admission.request.node_did.as_str()),
        "file inventory reply journal requires the source-local node"
    );
    Ok(())
}

/// The journal retains a past signed outcome, not current permission to
/// deliver it. Callers must recheck the existing delivery authorization owner.
pub async fn load_recorded_reply(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    verifier: &dyn NodeIdentity,
) -> Result<Option<FileInventoryReply>> {
    require_source(node, demand)?;
    ConfigAccess::transact_local_readonly(node, None, "file_inventory.load_reply", |txn| {
        Box::pin(load_in_txn(txn, demand, verifier))
    })
    .await
}

pub async fn record_reply(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    candidate: &FileInventoryReply,
    verifier: &dyn NodeIdentity,
) -> Result<FileInventoryReply> {
    require_source(node, demand)?;
    ConfigAccess::transact_local_write(node, None, "file_inventory.record_reply", |txn| {
        Box::pin(record_in_txn(txn, demand, candidate, verifier))
    })
    .await
}

async fn load_in_txn(
    txn: &ConfigApplyTxn<'_>,
    demand: &FileInventoryPageDemand,
    verifier: &dyn NodeIdentity,
) -> Result<Option<FileInventoryReply>> {
    let key = demand.derive_request_key()?;
    let response = txn
        .execute_local_response(&format!(
            r#"{{ FileInventoryReply(filter: {{ request_key: {{ _eq: "{}" }} }}, limit: 2) {{
            _docID request_key node_did demand reply }} }}"#,
            escape_graphql_string(&key)
        ))
        .await?;
    let rows = crate::graphql::required_rows::<serde_json::Value>(&response, "FileInventoryReply")?;
    ensure!(
        rows.len() <= 1,
        "file inventory recorded reply is ambiguous"
    );
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let stored_demand: FileInventoryPageDemand = serde_json::from_value(row["demand"].clone())?;
    ensure!(
        stored_demand == *demand
            && row["request_key"].as_str() == Some(key.as_str())
            && row["node_did"].as_str() == Some(demand.admission.request.node_did.as_str()),
        "file inventory recorded reply crossed source or demand scope"
    );
    let id = row["_docID"]
        .as_str()
        .filter(|id| !id.is_empty())
        .context("file inventory recorded reply omitted its identity")?
        .to_owned();
    let owners = txn
        .document_owners("FileInventoryReply", std::slice::from_ref(&id))
        .await?;
    ensure!(
        owners.len() == 1
            && owners[0].doc_id == id
            && owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == demand.admission.request.node_did),
        "file inventory recorded reply is not owned by its source"
    );
    let reply: FileInventoryReply = serde_json::from_value(row["reply"].clone())?;
    super::verify_reply(demand, &reply, verifier).await?;
    Ok(Some(reply))
}

pub(crate) async fn record_in_txn(
    txn: &ConfigApplyTxn<'_>,
    demand: &FileInventoryPageDemand,
    candidate: &FileInventoryReply,
    verifier: &dyn NodeIdentity,
) -> Result<FileInventoryReply> {
    if let Some(prior) = load_in_txn(txn, demand, verifier).await? {
        return Ok(prior);
    }
    super::verify_reply(demand, candidate, verifier).await?;
    let result = txn.execute_with_variables(
        "mutation($input: FileInventoryReplyMutationInputArg!) { create_FileInventoryReply(input: $input) { _docID } }",
        &serde_json::json!({"input": {
            "request_key": demand.derive_request_key()?,
            "node_did": demand.admission.request.node_did,
            "demand": demand,
            "reply": candidate,
        }}),
    ).await?;
    ensure!(
        crate::graphql::mutation_documents(&result, "create_FileInventoryReply")?
            .is_some_and(|rows| rows.len() == 1 && rows[0]["_docID"].as_str().is_some()),
        "file inventory reply was not retained"
    );
    Ok(candidate.clone())
}
