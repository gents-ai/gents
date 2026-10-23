use std::sync::Arc;

use anyhow::{ensure, Result};
use defra_node::EmbeddedNode;
use gents_protocol::file_inventory::{
    FileInventoryPageDemand, FileInventoryPageGrant, FileInventoryPageReceipt, FileInventoryReply,
    FileInventoryResponse, FILE_INVENTORY_PAGE_GRANT_VERSION,
};
use serde::Deserialize;

use crate::config_client::{ConfigAccess, ConfigApplyTxn};
use crate::graphql::{escape_graphql_string, required_rows};
use crate::NodeIdentity;

#[derive(Debug, Clone)]
pub(crate) struct PreparedInventoryPage {
    pub document_id: String,
    pub receipt: FileInventoryPageReceipt,
}

#[derive(Deserialize)]
struct PageRow {
    #[serde(rename = "_docID")]
    document_id: String,
    request_key: String,
    node_did: String,
    demand: FileInventoryPageDemand,
    receipt: FileInventoryPageReceipt,
}

pub(crate) async fn load_prepared_in_txn(
    txn: &ConfigApplyTxn<'_>,
    demand: &FileInventoryPageDemand,
    verifier: &dyn NodeIdentity,
) -> Result<Option<PreparedInventoryPage>> {
    let key = demand.derive_request_key()?;
    let response = txn
        .execute_local_response(&format!(
            r#"{{ FileInventoryPage(filter: {{ request_key: {{ _eq: "{}" }} }}, limit: 2) {{
        _docID request_key node_did demand receipt }} }}"#,
            escape_graphql_string(&key)
        ))
        .await?;
    let mut rows = required_rows::<PageRow>(&response, "FileInventoryPage")?;
    ensure!(rows.len() <= 1, "prepared file inventory page is ambiguous");
    let Some(row) = rows.pop() else {
        return Ok(None);
    };
    ensure!(
        !row.document_id.is_empty()
            && row.request_key == key
            && row.demand == *demand
            && row.node_did == demand.admission.request.node_did,
        "prepared file inventory page crossed source or demand scope"
    );
    let owners = txn
        .document_owners("FileInventoryPage", std::slice::from_ref(&row.document_id))
        .await?;
    ensure!(
        owners.len() == 1
            && owners[0].doc_id == row.document_id
            && owners[0]
                .owner
                .as_ref()
                .is_some_and(|owner| owner.as_str() == row.node_did),
        "prepared file inventory page is not owned by its source"
    );
    super::verify_reply(
        demand,
        &FileInventoryReply::Page(row.receipt.clone()),
        verifier,
    )
    .await?;
    Ok(Some(PreparedInventoryPage {
        document_id: row.document_id,
        receipt: row.receipt,
    }))
}

pub(crate) async fn load_prepared_page(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    verifier: &dyn NodeIdentity,
) -> Result<Option<PreparedInventoryPage>> {
    ensure!(
        node.node_identity_did() == Some(demand.admission.request.node_did.as_str()),
        "file inventory preparation requires the source node"
    );
    ConfigAccess::transact_local_readonly(node, None, "file_inventory.load_prepared", |txn| {
        Box::pin(load_prepared_in_txn(txn, demand, verifier))
    })
    .await
}

/// Preparation is immutable but not terminal. Lost acknowledgments retry this
/// exact document; no reader or automatic replication route is installed here.
pub(crate) async fn prepare_page(
    node: &Arc<EmbeddedNode>,
    demand: &FileInventoryPageDemand,
    candidate: &FileInventoryPageReceipt,
    verifier: &dyn NodeIdentity,
) -> Result<PreparedInventoryPage> {
    ensure!(
        node.node_identity_did() == Some(demand.admission.request.node_did.as_str()),
        "file inventory preparation requires the source node"
    );
    ConfigAccess::transact_local_write(node, None, "file_inventory.prepare_page", |txn| {
        Box::pin(prepare_page_in_txn(txn, demand, candidate, verifier))
    })
    .await
}

pub(super) async fn prepare_page_in_txn(
    txn: &ConfigApplyTxn<'_>,
    demand: &FileInventoryPageDemand,
    candidate: &FileInventoryPageReceipt,
    verifier: &dyn NodeIdentity,
) -> Result<PreparedInventoryPage> {
    if let Some(prior) = load_prepared_in_txn(txn, demand, verifier).await? {
        return Ok(prior);
    }
    super::verify_reply(
        demand,
        &FileInventoryReply::Page(candidate.clone()),
        verifier,
    )
    .await?;
    let response = txn.execute_with_variables(
                "mutation($input: FileInventoryPageMutationInputArg!) { create_FileInventoryPage(input: $input) { _docID } }",
                &serde_json::json!({"input": { "request_key": demand.derive_request_key()?,
                    "node_did": demand.admission.request.node_did, "demand": demand, "receipt": candidate }}),
            ).await?;
    Ok(PreparedInventoryPage {
        document_id: crate::graphql::created_doc_id(&response, "FileInventoryPage")?,
        receipt: candidate.clone(),
    })
}

pub(crate) async fn sign_page_grant(
    identity: &dyn NodeIdentity,
    demand: &FileInventoryPageDemand,
    prepared: &PreparedInventoryPage,
) -> Result<FileInventoryPageGrant> {
    ensure!(
        identity.did() == demand.admission.request.node_did && !prepared.document_id.is_empty(),
        "file inventory page grant requires its exact source and document"
    );
    super::verify_reply(
        demand,
        &FileInventoryReply::Page(prepared.receipt.clone()),
        identity,
    )
    .await?;
    let mut grant = FileInventoryPageGrant {
        version: FILE_INVENTORY_PAGE_GRANT_VERSION,
        demand: demand.clone(),
        document_id: prepared.document_id.clone(),
        signer_did: identity.did().to_owned(),
        signature: Vec::new(),
    };
    grant.signature = identity.sign(&grant.signing_payload()?).await?;
    Ok(grant)
}

pub struct VerifiedInventoryResponse(FileInventoryResponse);

impl VerifiedInventoryResponse {
    pub async fn verify(
        response: FileInventoryResponse,
        demand: &FileInventoryPageDemand,
        verifier: &dyn NodeIdentity,
    ) -> Result<Self> {
        demand.validate_reference()?;
        match &response {
            FileInventoryResponse::Refused(receipt) => {
                super::verify_reply(
                    demand,
                    &FileInventoryReply::Refused(receipt.clone()),
                    verifier,
                )
                .await?;
            }
            FileInventoryResponse::Page(grant) => {
                ensure!(
                    grant.version == FILE_INVENTORY_PAGE_GRANT_VERSION
                        && grant.demand == *demand
                        && !grant.document_id.is_empty()
                        && grant.signer_did == demand.admission.request.node_did,
                    "file inventory page grant crossed source, demand, document or purpose"
                );
                ensure!(
                    verifier
                        .verify(
                            &grant.signer_did,
                            &grant.signing_payload()?,
                            &grant.signature
                        )
                        .await?,
                    "file inventory page grant signature is invalid"
                );
            }
        }
        Ok(Self(response))
    }

    pub fn response(&self) -> &FileInventoryResponse {
        &self.0
    }

    /// Only a verified pointer can authorize the exact metadata replica read.
    /// File-image access still requires its separate explicit image demand.
    pub async fn read_page(
        &self,
        node: Arc<EmbeddedNode>,
        verifier: &dyn NodeIdentity,
    ) -> Result<Option<FileInventoryPageReceipt>> {
        let FileInventoryResponse::Page(grant) = &self.0 else {
            return Ok(None);
        };
        let scope = &grant.demand.admission.request;
        if !crate::tool_call_lifecycle::grant_source_file_read(
            node.clone(),
            gents_protocol::schemas::FILE_INVENTORY_PAGE_NAME,
            &scope.node_did,
            &scope.requester_did,
            &grant.document_id,
        )
        .await?
        {
            return Ok(None);
        }
        let prepared = ConfigAccess::transact_local_readonly(
            &node,
            None,
            "file_inventory.read_delivered",
            |txn| Box::pin(load_prepared_in_txn(txn, &grant.demand, verifier)),
        )
        .await?;
        let Some(prepared) = prepared else {
            return Ok(None);
        };
        ensure!(
            prepared.document_id == grant.document_id,
            "file inventory metadata differs from its signed document reference"
        );
        Ok(Some(prepared.receipt))
    }
}
