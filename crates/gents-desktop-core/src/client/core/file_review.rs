use gents::config_client::ConfigAccess;
use gents::graphql::escape_graphql_string;
use gents::tool_call_lifecycle::{
    load_tool_file_change_observation, ToolFileChangeRead, VerifiedFileImageReceipt,
};
use gents::NodeIdentity;
use gents_protocol::file_changes::ToolFileChanges;
use gents_protocol::file_image_hydration::{
    FileImageDemand, FileImageHydrationReceipt, FileImageSelection, FILE_IMAGE_RECEIPT_VERSION,
};
use gents_protocol::session_hydration::HydrationRequest;
use serde_json::Value;

use super::bootstrap::normalize_required;
use super::writes::{ensure_peer_chat_ready_at, find_request_in_scope};
use super::{ClientCore, ClientCoreError};

type Result<T, E = ClientCoreError> = std::result::Result<T, E>;

const DEMAND_FIELDS: &str = "_docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id status status_detail observation_doc_id processed_at outcome_signer_did outcome_signature";

impl ClientCore {
    pub async fn tool_file_changes(
        &self,
        node_did: &str,
        session_id: &str,
        request_id: &str,
        tool_call_doc_id: &str,
        retry_id: Option<&str>,
    ) -> Result<Option<ToolFileChanges>> {
        let node_did = normalize_required("node_did", node_did)?;
        let session_id = normalize_required("session_id", session_id)?;
        let request_id = normalize_required("request_id", request_id)?;
        let tool_call_doc_id = normalize_required("tool_call_doc_id", tool_call_doc_id)?;
        let retry_id = retry_id.map(uuid::Uuid::parse_str).transpose()?;
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
        if !(row.session_id.as_deref() == Some(session_id)
            && row.requester_did == projection.requester_did)
        {
            return Err(ClientCoreError::FileChangesCrossedScope);
        }
        let request_doc_id = row
            .doc_id
            .as_deref()
            .ok_or(ClientCoreError::RequestLacksPhysicalIdentity)?;
        if let Some(graphql) = self.operator_observation_graphql(node_did) {
            return Ok(gents::tool_call_lifecycle::load_tool_call_file_changes(
                &ConfigAccess::Graphql(graphql),
                tool_call_doc_id,
                request_doc_id,
                node_did,
                session_id,
                row.requester_did.as_deref(),
            )
            .await?);
        }
        let requester_did = row
            .requester_did
            .as_deref()
            .ok_or(ClientCoreError::FileReviewLacksRequester)?;
        if requester_did != self.node_identity.did() {
            return Err(ClientCoreError::FileReviewRequesterDiffers);
        }
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
        demand.admission.request_key = match retry_id {
            Some(id) => demand.derive_retry_request_key(&id.to_string()),
            None => demand.derive_request_key(),
        };
        demand.validate_reference()?;
        if retry_id.is_none() {
            let access = self
                .store_access
                .lock()
                .await
                .as_ref()
                .ok_or(ClientCoreError::LocalStoreAccessUnavailable)?
                .clone();
            demand = resumable_image_demand(&access, &demand, &self.node_identity).await?;
        }
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
            .ok_or(ClientCoreError::LocalStoreAccessUnavailable)?
            .clone();
        let mut sync = self.sync_state.subscribe();
        let mut observed_demand = false;
        let read = async {
            loop {
                let response = access.execute(&demand_query(&demand)).await?;
                let row = demand_row(&response)?;
                if row.is_none() && observed_demand {
                    return Err(ClientCoreError::FileReviewDemandDisappeared);
                }
                let receipt = match row {
                    Some(row) => {
                        observed_demand = true;
                        verified_receipt(row, &demand, &self.node_identity).await?
                    }
                    None => None,
                };
                if let Some(receipt) = receipt {
                    match receipt.selection() {
                        FileImageSelection::Rejected => {
                            return Err(ClientCoreError::FileReviewRejected {
                                detail: receipt.detail().to_owned(),
                            })
                        }
                        FileImageSelection::Missing => return Ok(None),
                        FileImageSelection::Selected { document } => {
                            if receipt.grant_local_read(self.node.clone()).await? {
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
                                        if &document_id != document {
                                            return Err(
                                                ClientCoreError::FileReviewObservationDiffers,
                                            );
                                        }
                                        return Ok(Some(changes));
                                    }
                                    ToolFileChangeRead::Unsupported => {
                                        return Err(ClientCoreError::FileImageCollectionUnavailable)
                                    }
                                    ToolFileChangeRead::Missing | ToolFileChangeRead::Pending => {}
                                }
                            }
                        }
                    }
                }
                let records = self.sync_state.records();
                let peer = records
                    .iter()
                    .find(|record| record.node_did == demand.admission.node_did);
                ensure_peer_chat_ready_at(&demand.admission.node_did, peer, chrono::Utc::now())
                    .map_err(|source| ClientCoreError::FileReviewReconnect {
                        source: Box::new(source),
                    })?;
                if row.is_none() {
                    admit_image_demand(&self.node, &demand).await?;
                    observed_demand = true;
                    continue;
                }
                tokio::select! {
                    event = changes.recv() => { event.ok_or(ClientCoreError::FileReviewSubscriptionClosed)?; }
                    update = sync.changed() => { update.map_err(ClientCoreError::FileReviewRouteObservationClosed)?; }
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), read)
            .await
            .map_err(ClientCoreError::FileReviewWaiting)?
    }
}

async fn resumable_image_demand(
    access: &ConfigAccess,
    original: &FileImageDemand,
    identity: &dyn NodeIdentity,
) -> Result<FileImageDemand> {
    let fields = original
        .reference_fields()
        .into_iter()
        .filter(|(field, _)| *field != "request_key")
        .map(|(field, value)| format!(r#"{field}: {{ _eq: "{}" }}"#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!(
        r#"{{ FileImageHydrationRequest(filter: {{ {fields}, status: {{ _in: ["pending", "served"] }} }}) {{ {DEMAND_FIELDS} }} }}"#,
    );
    let response = access.execute(&query).await?;
    let rows = response["data"]["FileImageHydrationRequest"]
        .as_array()
        .ok_or(ClientCoreError::FileReviewLookupOmittedRows)?;
    let mut candidates = Vec::new();
    let mut selected_document = None;
    for row in rows {
        let mut demand = original.clone();
        demand.admission.request_key = row["request_key"]
            .as_str()
            .ok_or(ClientCoreError::FileReviewDemandOmittedKey)?
            .into();
        demand.validate_reference()?;
        let receipt = verified_receipt(row, &demand, identity).await?;
        let rank = match receipt.as_ref().map(VerifiedFileImageReceipt::selection) {
            Some(FileImageSelection::Selected { document }) => {
                if let Some(previous) = &selected_document {
                    if previous != document {
                        return Err(ClientCoreError::FileReviewConflictingImages);
                    }
                }
                selected_document = Some(document.clone());
                0
            }
            Some(FileImageSelection::Missing) => 1,
            None => 2,
            Some(FileImageSelection::Rejected) => {
                return Err(ClientCoreError::FileReviewLookupRejected)
            }
        };
        candidates.push((rank, demand));
    }
    candidates.sort_by(|(left_rank, left), (right_rank, right)| {
        left_rank
            .cmp(right_rank)
            .then_with(|| left.admission.request_key.cmp(&right.admission.request_key))
    });
    Ok(candidates
        .into_iter()
        .next()
        .map(|(_, demand)| demand)
        .unwrap_or_else(|| original.clone()))
}

fn demand_query(demand: &FileImageDemand) -> String {
    format!(
        r#"{{ FileImageHydrationRequest(filter: {{ request_key: {{ _eq: "{}" }} }}, limit: 2) {{ {DEMAND_FIELDS} }} }}"#,
        escape_graphql_string(&demand.admission.request_key)
    )
}

fn demand_row(response: &Value) -> Result<Option<&Value>, ClientCoreError> {
    let rows = response["data"]["FileImageHydrationRequest"]
        .as_array()
        .ok_or(ClientCoreError::FileReviewLookupOmittedRows)?;
    if rows.len() > 1 {
        return Err(ClientCoreError::FileReviewConflictingRows);
    }
    Ok(rows.first())
}

fn ensure_demand(row: &Value, demand: &FileImageDemand) -> Result<(), ClientCoreError> {
    for (field, expected) in demand.reference_fields() {
        if row[field].as_str() != Some(expected) {
            return Err(ClientCoreError::FileReviewDemandDiffers {
                field: field.to_owned(),
            });
        }
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
) -> Result<Option<VerifiedFileImageReceipt>> {
    ensure_demand(row, demand)?;
    let text = |field: &str| {
        row[field]
            .as_str()
            .ok_or_else(|| ClientCoreError::FileReviewReceiptOmitted {
                field: field.to_owned(),
            })
    };
    let selection = match text("status")? {
        "pending" => return Ok(None),
        "rejected" => {
            if !row["observation_doc_id"].is_null() {
                return Err(ClientCoreError::RejectedFileReviewSelectedImage);
            }
            FileImageSelection::Rejected
        }
        "served" => match &row["observation_doc_id"] {
            Value::Null => FileImageSelection::Missing,
            Value::String(document) => FileImageSelection::Selected {
                document: document.clone(),
            },
            _ => return Err(ClientCoreError::FileReviewInvalidObservationIdentity),
        },
        _ => return Err(ClientCoreError::FileReviewUnknownStatus),
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
    Ok(Some(
        VerifiedFileImageReceipt::verify(receipt, demand, identity).await?,
    ))
}

#[cfg(test)]
mod tests;
