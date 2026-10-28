use super::*;
use crate::agent::p2p_reconcile::file_image_hydration::{
    authorize_inventory_image_read, inventory_image_entry, select_inventory_file_images,
    InventoryImagePageObservation, InventoryImageReadObservation,
};
use crate::config_client::ConfigApplyTxn;
use crate::file_inventory::delivery::load_prepared_in_txn;
use crate::tool_call_lifecycle::load_tool_file_change_observation_in_txn;

struct SelectedInventoryImage {
    catalog: HydrationCatalog,
    page: InventoryImagePageObservation,
    read: InventoryImageReadObservation,
}

impl GraphqlHydrationStore {
    pub(super) async fn inventory_image_authorized(
        &self,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<bool> {
        ConfigAccess::transact_local_readonly(
            &self.node,
            None,
            "file_inventory.authorize_image",
            |txn| {
                Box::pin(async move {
                    Ok(self
                        .inventory_image_in_txn(txn, demand, catalog)
                        .await?
                        .is_some())
                })
            },
        )
        .await
    }

    pub(super) async fn inventory_image_selection(
        &self,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<FileImageSelection> {
        ConfigAccess::transact_local_readonly(
            &self.node,
            None,
            "file_inventory.select_image",
            |txn| {
                Box::pin(async move {
                    let Some(selected) = self.inventory_image_in_txn(txn, demand, catalog).await?
                    else {
                        return Ok(FileImageSelection::Rejected);
                    };
                    let entry = inventory_image_entry(demand, &selected.page)
                        .context("authorized inventory image lost its selected entry")?;
                    let observed = load_tool_file_change_observation_in_txn(
                        txn,
                        &entry.tool_call_doc_id,
                        &entry.request_doc_id,
                        &entry.scope.node_did,
                        &entry.scope.session_id,
                        Some(&entry.scope.requester_did),
                    )
                    .await?;
                    let ToolFileChangeRead::Retained { document_id, .. } = observed else {
                        return Ok(FileImageSelection::Rejected);
                    };
                    Ok(select_inventory_file_images(
                        demand,
                        &selected.catalog,
                        &selected.page,
                        &selected.read,
                        &[FileImageTool {
                            doc_id: entry.tool_call_doc_id.clone(),
                            request_doc_id: entry.request_doc_id.clone(),
                            scope: entry.scope.clone(),
                            accepted: true,
                            terminal: true,
                        }],
                        &[FileImageObservation {
                            doc_id: document_id,
                            tool_call_doc_id: entry.tool_call_doc_id.clone(),
                            request_doc_id: entry.request_doc_id.clone(),
                            scope: entry.scope.clone(),
                        }],
                    ))
                })
            },
        )
        .await
    }

    /// The page signature, physical root and both ACP observations share the
    /// source snapshot. No image bytes are read until this returns authority.
    async fn inventory_image_in_txn(
        &self,
        txn: &ConfigApplyTxn<'_>,
        demand: &FileImageDemand,
        catalog: &HydrationCatalog,
    ) -> Result<Option<SelectedInventoryImage>> {
        demand.validate_reference()?;
        let reference = demand
            .inventory_reference
            .as_ref()
            .context("inventory image demand omitted its signed page reference")?;
        let Some(prepared) =
            load_prepared_in_txn(txn, &reference.page, self.identity.as_ref()).await?
        else {
            return Ok(None);
        };
        let page = InventoryImagePageObservation {
            owner: self.identity.did().to_owned(),
            signature_verified: true,
            receipt: prepared.receipt,
        };
        let Some(entry) = inventory_image_entry(demand, &page) else {
            return Ok(None);
        };
        let response = txn
            .execute_local_response(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}) {{
                _docID session_id requester_did node_did
            }} }}"#,
                escape_graphql_string(&demand.admission.session_id),
            ))
            .await?;
        let (sessions, session_reads) = super::super::observe_session_reader(
            txn,
            &demand.admission,
            &reference.page.admission.session_doc_id,
            rows::<SessionRow>(&response, "AgentSession")?,
            true,
        )
        .await?;
        let current = HydrationCatalog {
            sessions,
            session_reads,
            ..catalog.clone()
        };
        let reader: identity::Did = demand.admission.requester_did.parse()?;
        let projections = txn
            .document_owners_as(
                &reader,
                "ToolFileInventory",
                std::slice::from_ref(&entry.projection_doc_id),
            )
            .await?;
        let images = txn
            .document_owners_as(
                &reader,
                "ToolFileChange",
                std::slice::from_ref(&entry.observation_doc_id),
            )
            .await?;
        let projection = projections.first().filter(|observation| {
            projections.len() == 1 && observation.doc_id == entry.projection_doc_id
        });
        let image = images.first().filter(|observation| {
            images.len() == 1 && observation.doc_id == entry.observation_doc_id
        });
        let read = InventoryImageReadObservation {
            admission: reference.page.admission.clone(),
            projection_doc_id: entry.projection_doc_id.clone(),
            observation_doc_id: entry.observation_doc_id.clone(),
            projection_visible: projection.is_some_and(|observation| observation.visible),
            image_visible: image.is_some_and(|observation| observation.visible),
            projection_owner: projection
                .and_then(|observation| observation.owner.as_ref())
                .map(ToString::to_string)
                .unwrap_or_default(),
            image_owner: image
                .and_then(|observation| observation.owner.as_ref())
                .map(ToString::to_string)
                .unwrap_or_default(),
        };
        if authorize_inventory_image_read(demand, &current, &page, &read).is_none() {
            return Ok(None);
        }
        Ok(Some(SelectedInventoryImage {
            catalog: current,
            page,
            read,
        }))
    }
}
