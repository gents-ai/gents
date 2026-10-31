use super::*;
use crate::agent::p2p_reconcile::file_image_hydration::{
    completed_image_selection, image_attempt_delivery_target, record_image_outcome,
    select_file_images, FileImageContent, FileImageDemand, FileImageObservation, FileImageScope,
    FileImageSelection, FileImageTool,
};
use crate::store::config_client::ConfigAccess;
use crate::tools::call::lifecycle::{load_tool_file_change_observation, ToolFileChangeRead};
use gents_protocol::file_image_hydration::{
    FileImageHydrationReceipt, FileImageInventoryReference,
};
use gents_protocol::schemas::{
    FILE_IMAGE_HYDRATION_REQUEST_NAME, FILE_INVENTORY_IMAGE_REQUEST_NAME,
};

mod inventory;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ImageDemandRow {
    document_id: String,
    demand: FileImageDemand,
}

#[async_trait]
trait ImageRequestStore: Send + Sync {
    async fn pending_images(&self) -> Result<Vec<ImageDemandRow>>;
    async fn prior(&self, row: &ImageDemandRow) -> Result<Option<FileImageHydrationReceipt>>;
    async fn admission(&self, demand: &FileImageDemand) -> Result<LoadedHydrationAdmission>;
    /// The selection and the file documents its observation references.
    async fn selection(
        &self,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<(FileImageSelection, Vec<String>)>;
    async fn authorized(
        &self,
        demand: &FileImageDemand,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool>;
    async fn commit(
        &self,
        row: &ImageDemandRow,
        selection: &FileImageSelection,
        detail: &str,
    ) -> Result<()>;
}

pub(super) async fn reconcile_file_images(
    store: &GraphqlHydrationStore,
    delivery: &dyn HydrationDelivery,
) -> Result<()> {
    reconcile_images(store, delivery).await
}

async fn reconcile_images(
    store: &dyn ImageRequestStore,
    delivery: &dyn HydrationDelivery,
) -> Result<()> {
    let mut first_error = None;
    for row in store.pending_images().await? {
        if let Err(error) = serve_image(store, delivery, &row).await {
            tracing::warn!(request_key = %row.demand.admission.request_key, %error,
                "file-image hydration failed; continuing sweep");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn serve_image(
    store: &dyn ImageRequestStore,
    delivery: &dyn HydrationDelivery,
    row: &ImageDemandRow,
) -> Result<()> {
    let demand = &row.demand;
    let prior = store.prior(row).await?;
    if let Some(receipt) = &prior {
        return store.commit(row, &receipt.selection, &receipt.detail).await;
    }
    let admission = store.admission(demand).await?;
    let verdict = match &demand.inventory_reference {
        Some(reference) => super::super::session_hydration::decide_inventory_hydration(
            &reference.page.admission,
            &admission.catalog,
        ),
        None => decide_hydration(&demand.admission, &admission.catalog),
    };
    if demand.validate_reference().is_err() || matches!(verdict, HydrationVerdict::Reject(_)) {
        return commit_image_result(
            store,
            row,
            &FileImageSelection::Rejected,
            false,
            false,
            HydrationDeliveryResult::Confirmed,
        )
        .await;
    }
    let fence = admission
        .authorization
        .context("image demand has no authenticated enrollment fence")?;
    let (selection, contents) = store.selection(demand, &admission.catalog).await?;
    let before = store.authorized(demand, &fence).await?;
    let delivered = match image_attempt_delivery_target(
        prior.as_ref().map(|receipt| &receipt.selection),
        &selection,
        before,
    ) {
        Some(document) => {
            deliver_with_bounded_retry(
                delivery,
                &demand.admission,
                DeliveryTarget::FileImage(document, &contents),
            )
            .await
        }
        None => HydrationDeliveryResult::Confirmed,
    };
    if before
        && !matches!(selection, FileImageSelection::Rejected)
        && delivered == HydrationDeliveryResult::Indeterminate
    {
        return Ok(());
    }
    let after = store.authorized(demand, &fence).await?;
    commit_image_result(store, row, &selection, before, after, delivered).await
}

async fn commit_image_result(
    store: &dyn ImageRequestStore,
    row: &ImageDemandRow,
    selection: &FileImageSelection,
    before: bool,
    after: bool,
    delivered: HydrationDeliveryResult,
) -> Result<()> {
    let Some(terminal) = completed_image_selection(
        selection,
        before,
        after,
        delivered,
        HydrationTerminalWriteResult::Committed,
    ) else {
        return Ok(());
    };
    let detail = if matches!(terminal, FileImageSelection::Rejected) {
        "file review is not authorized for the selected physical tool"
    } else {
        ""
    };
    store
        .commit(row, &terminal, detail)
        .await
        .context("commit file-image hydration receipt")
}

#[derive(Deserialize)]
struct PendingImageRow {
    #[serde(rename = "_docID")]
    document_id: String,
    request_key: Option<String>,
    peer_id: Option<String>,
    requester_did: Option<String>,
    node_did: String,
    session_id: Option<String>,
    request_doc_id: Option<String>,
    tool_call_doc_id: Option<String>,
    inventory_reference: Option<serde_json::Value>,
}

#[async_trait]
impl ImageRequestStore for GraphqlHydrationStore {
    async fn prior(&self, row: &ImageDemandRow) -> Result<Option<FileImageHydrationReceipt>> {
        let response = ConfigAccess::local(self.node.clone())
            .execute(&recorded_image_query(row))
            .await?;
        decode_recorded_image_receipt(&response, row, self.identity.as_ref()).await
    }

    async fn pending_images(&self) -> Result<Vec<ImageDemandRow>> {
        let mut pending = Vec::new();
        for collection in [
            FILE_IMAGE_HYDRATION_REQUEST_NAME,
            FILE_INVENTORY_IMAGE_REQUEST_NAME,
        ] {
            let reference_field = if collection == FILE_INVENTORY_IMAGE_REQUEST_NAME {
                "inventory_reference"
            } else {
                ""
            };
            let query = format!(
                r#"{{ FileImageHydrationRequest: {collection}(filter: {{
                    status: {{ _eq: "pending" }}, node_did: {{ _eq: "{}" }}
                }}) {{ _docID request_key peer_id requester_did node_did session_id request_doc_id tool_call_doc_id {reference_field} }} }}"#,
                escape_graphql_string(self.identity.did()),
            );
            let response = graphql_with_transaction_retry(
                &self.node,
                &query,
                "query pending file-image hydration demands",
            )
            .await?;
            for row in rows::<PendingImageRow>(&response, FILE_IMAGE_HYDRATION_REQUEST_NAME)? {
                anyhow::ensure!(
                    !row.document_id.is_empty(),
                    "file-image demand omitted physical identity"
                );
                let reference = match row
                    .inventory_reference
                    .map(serde_json::from_value::<FileImageInventoryReference>)
                    .transpose()
                {
                    Ok(reference) => reference,
                    Err(error) => {
                        tracing::warn!(document = %row.document_id, collection, %error,
                            "file image demand has an invalid inventory reference");
                        continue;
                    }
                };
                let demand = FileImageDemand {
                    inventory_reference: reference,
                    admission: HydrationRequest {
                        request_key: row.request_key.unwrap_or_default(),
                        peer_id: row.peer_id.unwrap_or_default(),
                        requester_did: row.requester_did.unwrap_or_default(),
                        node_did: row.node_did,
                        session_id: row.session_id.unwrap_or_default(),
                    },
                    request_doc_id: row.request_doc_id.unwrap_or_default(),
                    tool_call_doc_id: row.tool_call_doc_id.unwrap_or_default(),
                };
                if demand.collection_name() != collection {
                    tracing::warn!(document = %row.document_id, collection,
                        "file image demand omitted its purpose binding");
                    continue;
                }
                pending.push(ImageDemandRow {
                    document_id: row.document_id,
                    demand,
                });
            }
        }
        Ok(pending)
    }

    async fn admission(&self, demand: &FileImageDemand) -> Result<LoadedHydrationAdmission> {
        self.load_source_admission(
            &demand.admission,
            demand.collection_name(),
            demand
                .inventory_reference
                .as_ref()
                .map(|reference| reference.page.admission.session_doc_id.as_str()),
        )
        .await
    }

    async fn selection(
        &self,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<(FileImageSelection, Vec<String>)> {
        if demand.inventory_reference.is_some() {
            return self.inventory_image_selection(demand, catalog).await;
        }
        let request = &demand.admission;
        let observed = load_tool_file_change_observation(
            &ConfigAccess::local(self.node.clone()),
            &demand.tool_call_doc_id,
            &demand.request_doc_id,
            &request.node_did,
            &request.session_id,
            Some(&request.requester_did),
        )
        .await?;
        let scope = FileImageScope {
            requester_did: request.requester_did.clone(),
            node_did: request.node_did.clone(),
            session_id: request.session_id.clone(),
        };
        let (terminal, images) = match observed {
            ToolFileChangeRead::Pending => (false, Vec::new()),
            ToolFileChangeRead::Unsupported => {
                anyhow::bail!("runtime file-image collection is unavailable")
            }
            ToolFileChangeRead::Missing => (true, Vec::new()),
            ToolFileChangeRead::Retained {
                document_id,
                contents,
                ..
            } => (
                true,
                vec![FileImageObservation {
                    doc_id: document_id,
                    tool_call_doc_id: demand.tool_call_doc_id.clone(),
                    request_doc_id: demand.request_doc_id.clone(),
                    scope: scope.clone(),
                    contents: contents
                        .into_iter()
                        .map(|content| FileImageContent {
                            doc_id: content.doc_id,
                            tool_call_doc_id: content.tool_call_doc_id,
                            scope: FileImageScope {
                                requester_did: content.scope.requester_did.unwrap_or_default(),
                                node_did: content.scope.node_did,
                                session_id: content.scope.session_id,
                            },
                        })
                        .collect(),
                }],
            ),
        };
        let selection = select_file_images(
            demand,
            catalog,
            &[FileImageTool {
                doc_id: demand.tool_call_doc_id.clone(),
                request_doc_id: demand.request_doc_id.clone(),
                scope,
                accepted: true,
                terminal,
            }],
            &images,
        );
        let contents = match &selection {
            FileImageSelection::Selected { .. } => images
                .first()
                .map(|image| image.delivery_documents().split_off(1))
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        Ok((selection, contents))
    }

    async fn authorized(
        &self,
        demand: &FileImageDemand,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool> {
        if !self
            .authorization_is_current(&demand.admission, fence)
            .await?
        {
            return Ok(false);
        }
        if demand.inventory_reference.is_some() {
            let current = self.admission(demand).await?;
            if current.authorization.as_ref() != Some(fence) {
                return Ok(false);
            }
            return self
                .inventory_image_authorized(demand, &current.catalog)
                .await;
        }
        Ok(true)
    }

    async fn commit(
        &self,
        row: &ImageDemandRow,
        selection: &FileImageSelection,
        detail: &str,
    ) -> Result<()> {
        let mut receipt = FileImageHydrationReceipt {
            version: row.demand.receipt_version(),
            demand: row.demand.clone(),
            selection: selection.clone(),
            detail: detail.to_owned(),
            processed_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
            signer_did: self.identity.did().to_owned(),
            signature: Vec::new(),
        };
        anyhow::ensure!(
            receipt.signer_did == receipt.demand.admission.node_did,
            "file-image reconciler cannot sign for another node"
        );
        receipt.signature = self.identity.sign(&receipt.signing_payload()?).await?;
        receipt.validate_shape()?;
        let row = row.clone();
        let identity = self.identity.clone();
        ConfigAccess::transact_local_write(
            &self.node,
            None,
            "p2p.commit_file_image_receipt",
            move |txn| {
                let row = row.clone();
                let candidate = receipt.clone();
                let identity = identity.clone();
                Box::pin(async move {
                    let response = txn.execute(&recorded_image_query(&row)).await?;
                    let prior = decode_recorded_image_receipt(&response, &row, identity.as_ref()).await?;
                    let outcome = record_image_outcome(
                        prior.as_ref().map(|receipt| &receipt.selection),
                        &candidate.selection,
                        HydrationTerminalWriteResult::Committed,
                    ).context("file-image transaction did not select an outcome")?;
                    let new_outcome = prior.is_none();
                    let receipt = prior.unwrap_or(candidate);
                    anyhow::ensure!(receipt.selection == outcome, "file-image receipt changed its recorded outcome");
                    if new_outcome {
                        txn.execute_with_variables(
                            "mutation($input: FileImageHydrationReceiptMutationInputArg!) { create_FileImageHydrationReceipt(input: $input) { _docID } }",
                            &serde_json::json!({"input": {
                                "request_key": receipt.demand.admission.request_key,
                                "node_did": receipt.signer_did,
                                "receipt": serde_json::to_value(&receipt)?,
                            }}),
                        ).await?;
                    }
                    let variables = match &receipt.demand.inventory_reference {
                        Some(reference) => serde_json::json!({"inventoryReference": reference}),
                        None => serde_json::json!({}),
                    };
                    let response = txn.execute_with_variables(&terminal_image_mutation(&row, &receipt)?, &variables).await?;
                    anyhow::ensure!(
                        response["data"]["update_FileImageHydrationRequest"].as_array().is_some_and(|rows| rows.len() == 1),
                        "file-image receipt lost its pending-row compare-and-set"
                    );
                    Ok(())
                })
            },
        )
        .await
    }
}

fn recorded_image_query(row: &ImageDemandRow) -> String {
    format!(
        r#"{{ FileImageHydrationReceipt(filter: {{ request_key: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }} }}) {{ receipt }} }}"#,
        escape_graphql_string(&row.demand.admission.request_key),
        escape_graphql_string(&row.demand.admission.node_did),
    )
}

async fn decode_recorded_image_receipt(
    response: &serde_json::Value,
    row: &ImageDemandRow,
    identity: &dyn NodeIdentity,
) -> Result<Option<FileImageHydrationReceipt>> {
    let rows = response["data"]["FileImageHydrationReceipt"]
        .as_array()
        .context("file-image receipt query omitted its rows")?;
    anyhow::ensure!(rows.len() <= 1, "recorded file-image outcome is ambiguous");
    let Some(stored) = rows.first() else {
        return Ok(None);
    };
    let receipt: FileImageHydrationReceipt = serde_json::from_value(stored["receipt"].clone())?;
    receipt.validate_shape()?;
    anyhow::ensure!(
        receipt.demand == row.demand && receipt.signer_did == identity.did(),
        "recorded file-image receipt crossed source or demand scope"
    );
    anyhow::ensure!(
        identity
            .verify(
                &receipt.signer_did,
                &receipt.signing_payload()?,
                &receipt.signature
            )
            .await?,
        "recorded file-image receipt has an invalid source signature"
    );
    Ok(Some(receipt))
}

fn terminal_image_mutation(
    row: &ImageDemandRow,
    receipt: &FileImageHydrationReceipt,
) -> Result<String> {
    anyhow::ensure!(
        row.demand == receipt.demand,
        "file-image receipt crossed demand scope"
    );
    receipt.validate_shape()?;
    let mut filters = vec![format!(
        "_docID: {{ _eq: \"{}\" }}",
        escape_graphql_string(&row.document_id)
    )];
    let demand = &receipt.demand;
    for (key, value) in demand.reference_fields() {
        filters.push(format!(
            "{key}: {{ _eq: \"{}\" }}",
            escape_graphql_string(value)
        ));
    }
    if demand.inventory_reference.is_some() {
        filters.push("inventory_reference: { _eq: $inventoryReference }".into());
    }
    let collection = demand.collection_name();
    let operation = if demand.inventory_reference.is_some() {
        "mutation($inventoryReference: JSON)"
    } else {
        "mutation"
    };
    filters.push("status: { _eq: \"pending\" }".into());
    let (status, observation) = match &receipt.selection {
        FileImageSelection::Rejected => ("rejected", "null".into()),
        FileImageSelection::Missing => ("served", "null".into()),
        FileImageSelection::Selected { document } => {
            ("served", format!("\"{}\"", escape_graphql_string(document)))
        }
    };
    Ok(format!(
        r#"{operation} {{ update_FileImageHydrationRequest: update_{collection}(filter: {{ {} }}, input: {{
        status: "{status}", status_detail: "{}", observation_doc_id: {observation},
        processed_at: "{}", outcome_signer_did: "{}", outcome_signature: "{}"
    }}) {{ _docID }} }}"#,
        filters.join(", "),
        escape_graphql_string(&receipt.detail),
        escape_graphql_string(&receipt.processed_at),
        escape_graphql_string(&receipt.signer_did),
        escape_graphql_string(&bs58::encode(&receipt.signature).into_string())
    ))
}

#[cfg(test)]
mod tests;
