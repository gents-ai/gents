use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_inventory::{FileInventoryPageDemand, FileInventoryResponse};
use serde::Deserialize;

use crate::store::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::store::graphql::{escape_graphql_string, required_rows};

pub(crate) const REQUEST_FIELDS: &str =
    "_docID request_key peer_id requester_did node_did session_id demand status response";

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct InventoryRequestRow {
    #[serde(rename = "_docID")]
    pub document_id: String,
    pub request_key: String,
    pub peer_id: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
    pub demand: FileInventoryPageDemand,
    pub status: String,
    pub response: Option<FileInventoryResponse>,
}

impl InventoryRequestRow {
    pub fn validate(&self) -> Result<()> {
        let scope = &self.demand.admission.request;
        ensure!(
            !self.document_id.is_empty()
                && self.request_key == self.demand.derive_request_key()?
                && self.peer_id == scope.peer_id
                && self.requester_did == scope.requester_did
                && self.node_did == scope.node_did
                && self.session_id == scope.session_id,
            "file inventory request crossed its immutable demand or route"
        );
        ensure!(
            match (&self.response, self.status.as_str()) {
                (None, "pending") => true,
                (Some(response), status) => status == response_status(response),
                _ => false,
            },
            "file inventory request has an invalid response state"
        );
        Ok(())
    }
}

/// Local admission commits the immutable attempt before its filtered control
/// route can publish it. Reload retries the exact key, never a new attempt.
pub async fn admit_demand(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
) -> Result<String> {
    demand.validate_reference()?;
    ensure!(
        node.node_identity_did() == Some(demand.admission.request.requester_did.as_str()),
        "file inventory admission requires the requesting client"
    );
    ConfigAccess::transact_local_write(node, None, "file_inventory.admit_demand", |txn| {
        Box::pin(async move {
            if let Some(row) = load_request_in_txn(txn, demand).await? {
                return Ok(row.document_id);
            }
            let scope = &demand.admission.request;
            let result = txn.execute_with_variables(
                "mutation($input: FileInventoryRequestMutationInputArg!) { create_FileInventoryRequest(input: $input) { _docID } }",
                &serde_json::json!({"input": {
                    "request_key": demand.derive_request_key()?, "peer_id": scope.peer_id,
                    "requester_did": scope.requester_did, "node_did": scope.node_did,
                    "session_id": scope.session_id, "demand": demand, "status": "pending",
                    "created_at": chrono::Utc::now().to_rfc3339(),
                }}),
            ).await?;
            Ok(crate::store::graphql::created_doc_id(&result, "FileInventoryRequest")?)
        })
    }).await
}

pub(crate) async fn load_request_in_txn(
    txn: &ConfigApplyTxn<'_>,
    demand: &FileInventoryPageDemand,
) -> Result<Option<InventoryRequestRow>> {
    let query = format!(
        r#"{{ FileInventoryRequest(filter: {{ request_key: {{ _eq: "{}" }} }}, limit: 2) {{ {REQUEST_FIELDS} }} }}"#,
        escape_graphql_string(&demand.derive_request_key()?)
    );
    let response = txn.execute_local_response(&query).await?;
    let mut rows = required_rows::<InventoryRequestRow>(&response, "FileInventoryRequest")?;
    ensure!(rows.len() <= 1, "file inventory request is ambiguous");
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    row.validate()?;
    ensure!(
        row.demand == *demand,
        "file inventory request reused another demand key"
    );
    Ok(Some(row))
}

fn response_status(response: &FileInventoryResponse) -> &'static str {
    match response {
        FileInventoryResponse::Page(_) => "served",
        FileInventoryResponse::Refused(receipt) => match receipt.reason {
            gents_protocol::file_inventory::FileInventoryRefusalReason::Rejected => "rejected",
            gents_protocol::file_inventory::FileInventoryRefusalReason::Stale => "stale",
            gents_protocol::file_inventory::FileInventoryRefusalReason::Unavailable => {
                "unavailable"
            }
        },
    }
}

pub(crate) async fn publish_response_in_txn(
    txn: &ConfigApplyTxn<'_>,
    row: &InventoryRequestRow,
    response: &FileInventoryResponse,
) -> Result<()> {
    row.validate()?;
    let filter = format!(
        r#"_docID: {{ _eq: "{}" }}, request_key: {{ _eq: "{}" }}, status: {{ _eq: "pending" }}"#,
        escape_graphql_string(&row.document_id),
        escape_graphql_string(&row.request_key)
    );
    let status = response_status(response);
    let result = txn.execute_with_variables(&format!(
        "mutation($input: FileInventoryRequestMutationInputArg!) {{ update_FileInventoryRequest(filter: {{ {filter} }}, input: $input) {{ _docID }} }}"),
        &serde_json::json!({"input": {"status": status, "response": response}})).await?;
    let rows = crate::store::graphql::mutation_documents(&result, "update_FileInventoryRequest")?
        .context("file inventory request update omitted its result")?;
    ensure!(
        rows.len() == 1 && rows[0]["_docID"].as_str() == Some(row.document_id.as_str()),
        "file inventory response lost its pending-row compare-and-set"
    );
    Ok(())
}

pub async fn verified_client_response(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    verifier: &dyn crate::NodeIdentity,
) -> Result<Option<super::VerifiedInventoryResponse>> {
    demand.validate_reference()?;
    ensure!(
        node.node_identity_did() == Some(demand.admission.request.requester_did.as_str()),
        "file inventory response requires the requesting client"
    );
    ConfigAccess::transact_local_readonly(node, None, "file_inventory.client_response", |txn| {
        Box::pin(async move {
            let Some(response) = load_request_in_txn(txn, demand)
                .await?
                .and_then(|row| row.response)
            else {
                return Ok(None);
            };
            super::VerifiedInventoryResponse::verify(response, demand, verifier)
                .await
                .map(Some)
        })
    })
    .await
}
