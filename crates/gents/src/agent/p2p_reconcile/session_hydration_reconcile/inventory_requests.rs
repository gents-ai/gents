use super::*;
use crate::config_client::ConfigAccess;
use crate::file_inventory::delivery::{
    load_prepared_in_txn, load_prepared_page, prepare_page, sign_page_grant,
};
use crate::file_inventory::requests::{
    publish_response_in_txn, InventoryRequestRow, REQUEST_FIELDS,
};
use gents_protocol::file_inventory::{
    FileInventoryPageDemand, FileInventoryRefusalReason, FileInventoryReply, FileInventoryResponse,
};
use gents_protocol::schemas::FILE_INVENTORY_REQUEST_NAME;

pub(super) async fn reconcile_inventory_requests(
    store: &GraphqlHydrationStore,
    delivery: &dyn HydrationDelivery,
) -> Result<()> {
    let filter = format!(
        r#"status: {{ _eq: "pending" }}, node_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(store.identity.did())
    );
    let pending =
        ConfigAccess::transact_local_readonly(&store.node, None, "file_inventory.pending", |txn| {
            let filter = &filter;
            Box::pin(async move {
                txn.cursor_rows(FILE_INVENTORY_REQUEST_NAME, REQUEST_FIELDS, filter)
                    .await
            })
        })
        .await?;
    let mut first_error = None;
    for value in pending {
        let result = async {
            let row: InventoryRequestRow = serde_json::from_value(value)?;
            row.validate()?;
            serve_inventory(store, delivery, &row).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error, "file inventory request failed; continuing sweep");
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

async fn refuse(
    store: &GraphqlHydrationStore,
    demand: &FileInventoryPageDemand,
    reason: FileInventoryRefusalReason,
) -> Result<FileInventoryReply> {
    Ok(FileInventoryReply::Refused(
        crate::file_inventory::sign_refusal(store.identity.as_ref(), demand, reason).await?,
    ))
}

async fn serve_inventory(
    store: &GraphqlHydrationStore,
    delivery: &dyn HydrationDelivery,
    row: &InventoryRequestRow,
) -> Result<()> {
    let demand = &row.demand;
    if let Some(prior) =
        crate::file_inventory::load_recorded_reply(&store.node, demand, store.identity.as_ref())
            .await?
    {
        return store.commit_inventory(row, &prior).await;
    }
    let admission = store.inventory_admission(demand).await?;
    if let HydrationVerdict::Reject(reason) =
        decide_hydration(&demand.admission.request, &admission.catalog)
    {
        tracing::debug!(request_key = %row.request_key, attempt_id = %demand.attempt_id, cursor_offset = ?demand.cursor.as_ref().map(|cursor| cursor.offset), reason, "file inventory admission refused");
        return store
            .commit_inventory(
                row,
                &refuse(store, demand, FileInventoryRefusalReason::Rejected).await?,
            )
            .await;
    }
    let fence = admission
        .authorization
        .context("inventory demand lacks its authenticated enrollment fence")?;
    let prepared = match load_prepared_page(&store.node, demand, store.identity.as_ref()).await? {
        Some(prepared) => prepared,
        None => {
            let snapshot = match FileInventorySnapshot::load(&store.node, &demand.admission).await {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::warn!(%error, attempt_id = %demand.attempt_id, cursor_offset = ?demand.cursor.as_ref().map(|cursor| cursor.offset), "file inventory snapshot is unavailable");
                    return store
                        .commit_inventory(
                            row,
                            &refuse(store, demand, super::inventory::refusal_reason(&error))
                                .await?,
                        )
                        .await;
                }
            };
            let page = match snapshot.page(demand.cursor.as_ref()) {
                Ok(page) => page,
                Err(error) => {
                    let reason = error.refusal_reason();
                    return store
                        .commit_inventory(row, &refuse(store, demand, reason).await?)
                        .await;
                }
            };
            let signed = match crate::file_inventory::sign_current_page(
                store.identity.as_ref(),
                &demand.derive_request_key()?,
                page,
                || store.inventory_current(demand, &fence),
            )
            .await
            {
                Ok(signed) => signed,
                Err(error) => {
                    tracing::warn!(%error, attempt_id = %demand.attempt_id, cursor_offset = ?demand.cursor.as_ref().map(|cursor| cursor.offset), "file inventory issuance did not retain authority");
                    return store
                        .commit_inventory(
                            row,
                            &refuse(store, demand, FileInventoryRefusalReason::Rejected).await?,
                        )
                        .await;
                }
            };
            prepare_page(&store.node, demand, &signed, store.identity.as_ref()).await?
        }
    };
    let before = store.inventory_current(demand, &fence).await?;
    let delivered = if before {
        deliver_with_bounded_retry(
            delivery,
            &demand.admission.request,
            DeliveryTarget::FileInventory(&prepared.document_id),
        )
        .await
    } else {
        HydrationDeliveryResult::Confirmed
    };
    let after = store.inventory_current(demand, &fence).await?;
    let candidate = FileInventoryReply::Page(prepared.receipt);
    if let Some(terminal) = crate::file_inventory::settle_reply(
        store.identity.as_ref(),
        demand,
        &candidate,
        crate::file_inventory::InventoryDeliveryObservation {
            before,
            after,
            delivery: delivered,
            write: HydrationTerminalWriteResult::Committed,
        },
    )
    .await?
    {
        store.commit_inventory(row, &terminal).await?;
    }
    Ok(())
}

impl GraphqlHydrationStore {
    pub(super) async fn inventory_admission(
        &self,
        demand: &FileInventoryPageDemand,
    ) -> Result<LoadedHydrationAdmission> {
        self.load_admission_for_collection(&demand.admission.request, FILE_INVENTORY_REQUEST_NAME)
            .await
    }

    pub(super) async fn inventory_current(
        &self,
        demand: &FileInventoryPageDemand,
        fence: &EnrollmentAuthorizationFence,
    ) -> Result<bool> {
        let current = self.inventory_admission(demand).await?;
        let verdict = decide_hydration(&demand.admission.request, &current.catalog);
        let same_authorization = current.authorization.as_ref() == Some(fence);
        let authorized = same_authorization && matches!(verdict, HydrationVerdict::Admit(_));
        if !authorized {
            let (verdict_kind, rejection, admitted_documents) = match &verdict {
                HydrationVerdict::Admit(documents) => ("admit", None, documents.len()),
                HydrationVerdict::Reject(reason) => ("reject", Some(*reason), 0),
            };
            tracing::debug!(
                attempt_id = %demand.attempt_id,
                same_authorization,
                before_sequence = fence.authorization_sequence,
                after_sequence = ?current.authorization.as_ref().map(|value| value.authorization_sequence),
                same_enrollment = current.authorization.as_ref().is_some_and(|value| value.request_id == fence.request_id),
                same_peer_binding = current.authorization.as_ref().is_some_and(|value| value.member_peer == fence.member_peer && value.member_ticket == fence.member_ticket),
                verdict = verdict_kind,
                ?rejection,
                admitted_documents,
                "file inventory authorization changed"
            );
        }
        Ok(authorized)
    }

    async fn commit_inventory(
        &self,
        row: &InventoryRequestRow,
        candidate: &FileInventoryReply,
    ) -> Result<()> {
        ConfigAccess::transact_local_write(
            &self.node,
            None,
            "file_inventory.commit_response",
            |txn| {
                Box::pin(async move {
                    let recorded = crate::file_inventory::journal::record_in_txn(
                        txn,
                        &row.demand,
                        candidate,
                        self.identity.as_ref(),
                    )
                    .await?;
                    let response = match recorded {
                        FileInventoryReply::Refused(receipt) => {
                            FileInventoryResponse::Refused(receipt)
                        }
                        FileInventoryReply::Page(receipt) => {
                            let prepared =
                                load_prepared_in_txn(txn, &row.demand, self.identity.as_ref())
                                    .await?
                                    .context("recorded inventory reply lacks its prepared page")?;
                            anyhow::ensure!(
                                prepared.receipt == receipt,
                                "recorded inventory reply changed its prepared page"
                            );
                            FileInventoryResponse::Page(
                                sign_page_grant(self.identity.as_ref(), &row.demand, &prepared)
                                    .await?,
                            )
                        }
                    };
                    publish_response_in_txn(txn, row, &response).await
                })
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests;
