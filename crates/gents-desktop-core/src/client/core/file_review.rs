use anyhow::{Context, Result};
use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{load_tool_file_change_observation, ToolFileChangeRead};
use gents::NodeIdentity;
use gents_protocol::file_changes::ToolFileChanges;
use gents_protocol::file_image_hydration::{
    FileImageDemand, FileImageHydrationReceipt, FileImageSelection, FILE_IMAGE_RECEIPT_VERSION,
};
use gents_protocol::session_hydration::HydrationRequest;
use serde_json::Value;

use super::bootstrap::normalize_required;
use super::writes::find_request_in_scope;
use super::ClientCore;

const DEMAND_FIELDS: &str = "_docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature";

impl ClientCore {
    pub async fn tool_file_changes(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
        tool_call_doc_id: &str,
    ) -> Result<Option<ToolFileChanges>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let request_id = normalize_required("request_id", request_id)?;
        let tool_call_doc_id = normalize_required("tool_call_doc_id", tool_call_doc_id)?;
        let Some(projection) = self
            .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
            .await?
        else {
            return Ok(None);
        };
        let Some(row) = find_request_in_scope(&projection.requests, request_id, Some(node_did))?
        else {
            return Ok(None);
        };
        anyhow::ensure!(
            row.session_id.as_deref() == Some(session_id)
                && row.requester_did == projection.requester_did,
            "file changes crossed the selected request scope"
        );
        let request_doc_id = row
            .doc_id
            .as_deref()
            .context("request lacks physical identity")?;
        if let Some(graphql) = self.operator_observation_graphql(node_did) {
            return gents::tool_call_lifecycle::load_tool_call_file_changes(
                &ConfigAccess::Graphql(graphql),
                tool_call_doc_id,
                request_doc_id,
                node_did,
                session_id,
                row.requester_did.as_deref(),
            )
            .await;
        }
        let requester_did = row
            .requester_did
            .as_deref()
            .context("file review lacks its requester")?;
        anyhow::ensure!(
            requester_did == self.node_identity.did(),
            "file review requester differs from the enrolled client"
        );
        let mut demand = FileImageDemand {
            admission: HydrationRequest {
                request_key: String::new(),
                peer_id: self.local_peer_id().into(),
                requester_did: requester_did.into(),
                node_did: node_did.into(),
                session_id: session_id.into(),
            },
            request_doc_id: request_doc_id.into(),
            tool_call_doc_id: tool_call_doc_id.into(),
        };
        demand.admission.request_key = demand.derive_request_key();
        demand.validate_reference()?;
        self.remote_file_images(demand).await
    }

    /// A terminal receipt confirms transport, while the scoped document-change
    /// subscription waits for the exact image's local merge. Dropping this read
    /// cancels the wait without changing the durable demand or its authority.
    async fn remote_file_images(&self, demand: FileImageDemand) -> Result<Option<ToolFileChanges>> {
        let mut changes = gents::subscribe_collections(
            &self.node,
            ["FileImageHydrationRequest", "ToolFileChange"],
        )?;
        let access = self
            .store_access
            .lock()
            .await
            .as_ref()
            .context("local store access is unavailable")?
            .clone();
        admit_image_demand(&self.node, &demand).await?;
        let read = async {
            loop {
                let response = access.execute(&demand_query(&demand)).await?;
                let row = demand_row(&response)?.context("file review demand disappeared")?;
                if let Some(receipt) = verified_receipt(row, &demand, &self.node_identity).await? {
                    match receipt.selection {
                        FileImageSelection::Rejected => {
                            anyhow::bail!("Runtime rejected file review: {}", receipt.detail)
                        }
                        FileImageSelection::Missing => return Ok(None),
                        FileImageSelection::Selected { document } => {
                            match load_tool_file_change_observation(
                                &access,
                                &demand.tool_call_doc_id,
                                &demand.request_doc_id,
                                &demand.admission.node_did,
                                &demand.admission.session_id,
                                Some(&demand.admission.requester_did),
                            )
                            .await?
                            {
                                ToolFileChangeRead::Retained {
                                    document_id,
                                    changes,
                                } => {
                                    anyhow::ensure!(
                                        document_id == document,
                                        "file review observation differs from the signed receipt"
                                    );
                                    return Ok(Some(changes));
                                }
                                ToolFileChangeRead::Unsupported => {
                                    anyhow::bail!("local file-image collection is unavailable")
                                }
                                ToolFileChangeRead::Missing | ToolFileChangeRead::Pending => {}
                            }
                        }
                    }
                }
                changes
                    .recv()
                    .await
                    .context("file review observation subscription closed")?;
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), read).await
            .context("File review is waiting for the runtime. Open the changes again when the connection is ready.")?
    }
}

fn demand_query(demand: &FileImageDemand) -> String {
    format!(
        r#"{{ FileImageHydrationRequest(filter: {{ request_key: {{ _eq: "{}" }} }}, limit: 2) {{ {DEMAND_FIELDS} }} }}"#,
        escape_graphql_string(&demand.admission.request_key)
    )
}

fn demand_row(response: &Value) -> Result<Option<&Value>> {
    let rows = response["data"]["FileImageHydrationRequest"]
        .as_array()
        .context("file review lookup omitted demand rows")?;
    anyhow::ensure!(rows.len() <= 1, "file review demand has conflicting rows");
    Ok(rows.first())
}

fn ensure_demand(row: &Value, demand: &FileImageDemand) -> Result<()> {
    for (field, expected) in demand.reference_fields() {
        anyhow::ensure!(
            row[field].as_str() == Some(expected),
            "file review demand {field} differs from the selected tool"
        );
    }
    Ok(())
}

async fn admit_image_demand(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    demand: &FileImageDemand,
) -> Result<()> {
    let query = demand_query(demand);
    let mut fields = Vec::new();
    for (field, value) in demand.reference_fields() {
        fields.push(format!("{field}: \"{}\"", escape_graphql_string(value)));
    }
    fields.push(format!(
        "created_at: \"{}\"",
        escape_graphql_string(&chrono::Utc::now().to_rfc3339())
    ));
    fields.push("status: \"pending\"".into());
    let mutation = format!(
        "mutation {{ create_FileImageHydrationRequest(input: {{ {} }}) {{ _docID }} }}",
        fields.join(", ")
    );
    let demand = demand.clone();
    ConfigAccess::transact_local_write(node, None, "desktop.file_review.admit", move |txn| {
        let query = query.clone();
        let mutation = mutation.clone();
        let demand = demand.clone();
        Box::pin(async move {
            gents::graphql::fence_arrivals(txn, "FileImageHydrationRequest").await?;
            let response = txn.execute(&query).await?;
            if let Some(row) = demand_row(&response)? {
                ensure_demand(row, &demand)?;
            } else {
                let response = txn.execute(&mutation).await?;
                gents::graphql::created_doc_id(&response, "FileImageHydrationRequest")?;
            }
            Ok(())
        })
    })
    .await
}

async fn verified_receipt(
    row: &Value,
    demand: &FileImageDemand,
    identity: &dyn NodeIdentity,
) -> Result<Option<FileImageHydrationReceipt>> {
    ensure_demand(row, demand)?;
    let text = |field: &str| {
        row[field]
            .as_str()
            .with_context(|| format!("file review receipt omitted {field}"))
    };
    let selection = match text("status")? {
        "pending" => return Ok(None),
        "rejected" => {
            anyhow::ensure!(
                row["observation_doc_id"].is_null(),
                "rejected file review selected an image"
            );
            FileImageSelection::Rejected
        }
        "served" => match &row["observation_doc_id"] {
            Value::Null => FileImageSelection::Missing,
            Value::String(document) => FileImageSelection::Selected {
                document: document.clone(),
            },
            _ => anyhow::bail!("file review receipt has an invalid observation identity"),
        },
        _ => anyhow::bail!("file review receipt has an unknown status"),
    };
    let receipt = FileImageHydrationReceipt {
        version: FILE_IMAGE_RECEIPT_VERSION,
        demand: demand.clone(),
        selection,
        detail: text("status_detail")?.into(),
        processed_at: text("processed_at")?.into(),
        signer_did: text("outcome_signer_did")?.into(),
        signature: bs58::decode(text("outcome_signature")?).into_vec()?,
    };
    receipt.validate_shape()?;
    anyhow::ensure!(
        identity
            .verify(
                &demand.admission.node_did,
                &receipt.signing_payload()?,
                &receipt.signature
            )
            .await?,
        "file review receipt signature is invalid"
    );
    Ok(Some(receipt))
}

#[cfg(test)]
mod tests;
