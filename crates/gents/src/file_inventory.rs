use anyhow::{ensure, Result};
use gents_protocol::file_inventory::{
    coherent_inventory_entries, FileInventoryAdmission, FileInventoryBinding, FileInventoryEntry,
    FileInventoryPageReceipt, FILE_INVENTORY_RECEIPT_VERSION,
};
use rapidhash::{HashSetExt, RapidHashSet};
use serde::{Deserialize, Serialize};

pub use crate::agent::p2p_reconcile::session_hydration_reconcile::{
    FileInventoryPageError, FileInventorySnapshot,
};
use crate::NodeIdentity;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceivedInventoryPage {
    pub request_key: String,
    pub receipt: FileInventoryPageReceipt,
}

/// Retain signed pages, not an unsigned materialized prefix. Reload rechecks
/// the exact nonce, purpose, source signature and contiguous snapshot binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryReceiver {
    pub admission: FileInventoryAdmission,
    pub pages: Vec<ReceivedInventoryPage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInventoryPrefix {
    binding: FileInventoryBinding,
    entries: Vec<FileInventoryEntry>,
    complete: bool,
}

impl FileInventoryPrefix {
    pub fn binding(&self) -> &FileInventoryBinding {
        &self.binding
    }

    pub fn entries(&self) -> &[FileInventoryEntry] {
        &self.entries
    }

    pub fn complete(&self) -> bool {
        self.complete
    }

    pub fn known_inventory(&self) -> bool {
        self.complete && self.binding.coverage.pending == 0 && self.binding.coverage.unknown == 0
    }
}

impl FileInventoryReceiver {
    pub async fn receive_for_demand(
        &self,
        demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
        receipt: FileInventoryPageReceipt,
        verifier: &dyn NodeIdentity,
    ) -> Result<Self> {
        ensure!(
            self.admission == demand.admission,
            "file inventory receiver does not own this demand"
        );
        demand.validate_page_reference(&receipt)?;
        self.receive(demand.derive_request_key()?, receipt, verifier)
            .await
    }

    pub async fn receive(
        &self,
        request_key: String,
        receipt: FileInventoryPageReceipt,
        verifier: &dyn NodeIdentity,
    ) -> Result<Self> {
        let mut next = self.clone();
        next.pages.push(ReceivedInventoryPage {
            request_key,
            receipt,
        });
        next.validated(verifier).await?;
        Ok(next)
    }

    pub async fn validated(
        &self,
        verifier: &dyn NodeIdentity,
    ) -> Result<Option<FileInventoryPrefix>> {
        let mut keys = RapidHashSet::new();
        let mut prefix: Option<FileInventoryPrefix> = None;
        for received in &self.pages {
            let receipt = &received.receipt;
            let page = &receipt.page;
            ensure!(
                !received.request_key.is_empty()
                    && keys.insert(&received.request_key)
                    && received.request_key == receipt.request_key
                    && receipt.version == FILE_INVENTORY_RECEIPT_VERSION
                    && receipt.signer_did == self.admission.request.node_did
                    && page.binding.admission == self.admission
                    && page.well_formed(),
                "file inventory page does not match its signed request or snapshot"
            );
            ensure!(
                verifier
                    .verify(
                        &self.admission.request.node_did,
                        &receipt.signing_payload()?,
                        &receipt.signature,
                    )
                    .await?,
                "file inventory page signature is invalid"
            );
            match &mut prefix {
                None => {
                    ensure!(page.offset == 0, "file inventory must begin at offset zero");
                    prefix = Some(FileInventoryPrefix {
                        binding: page.binding.clone(),
                        entries: page.entries.clone(),
                        complete: page.entries.len() as u64 == page.binding.coverage.entries,
                    });
                }
                Some(prior) => {
                    ensure!(
                        !prior.complete
                            && prior.binding == page.binding
                            && page.offset == prior.entries.len() as u64,
                        "file inventory pages crossed snapshots or left a gap"
                    );
                    prior.entries.extend(page.entries.iter().cloned());
                    ensure!(
                        coherent_inventory_entries(&prior.entries),
                        "file inventory prefix is incoherent"
                    );
                    prior.complete = prior.entries.len() as u64 == prior.binding.coverage.entries;
                }
            }
        }
        Ok(prefix)
    }
}

/// Authorization observations come only from the existing source owner. The
/// signed bytes stay local until the second observation succeeds.
pub(crate) async fn sign_current_page<F, Fut>(
    identity: &dyn NodeIdentity,
    request_key: &str,
    page: gents_protocol::file_inventory::FileInventoryPage,
    authorized: F,
) -> Result<FileInventoryPageReceipt>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<bool>>,
{
    ensure!(
        !request_key.is_empty()
            && page.well_formed()
            && identity.did() == page.binding.admission.request.node_did,
        "file inventory signing requires an exact source, nonce and well-formed page"
    );
    ensure!(
        authorized().await?,
        "file inventory authorization changed before signing"
    );
    let mut receipt = FileInventoryPageReceipt {
        version: FILE_INVENTORY_RECEIPT_VERSION,
        request_key: request_key.to_owned(),
        signer_did: identity.did().to_owned(),
        page,
        signature: Vec::new(),
    };
    receipt.signature = identity.sign(&receipt.signing_payload()?).await?;
    ensure!(
        authorized().await?,
        "file inventory authorization changed while signing"
    );
    Ok(receipt)
}

/// Verification is independent of transport status. A valid refusal settles
/// this demand without extending a page prefix or certifying an empty list.
pub async fn verify_reply(
    demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
    reply: &gents_protocol::file_inventory::FileInventoryReply,
    verifier: &dyn NodeIdentity,
) -> Result<()> {
    use gents_protocol::file_inventory::{FileInventoryReply, FILE_INVENTORY_REFUSAL_VERSION};
    let key = demand.derive_request_key()?;
    let (signer, payload, signature) = match reply {
        FileInventoryReply::Page(receipt) => {
            demand.validate_page_reference(receipt)?;
            ensure!(
                receipt.version == FILE_INVENTORY_RECEIPT_VERSION && receipt.page.well_formed(),
                "file inventory reply has an invalid page or purpose"
            );
            (
                &receipt.signer_did,
                receipt.signing_payload()?,
                &receipt.signature,
            )
        }
        FileInventoryReply::Refused(receipt) => {
            ensure!(
                receipt.version == FILE_INVENTORY_REFUSAL_VERSION
                    && receipt.demand == *demand
                    && receipt.request_key == key,
                "file inventory refusal does not match its demand or purpose"
            );
            (
                &receipt.signer_did,
                receipt.signing_payload()?,
                &receipt.signature,
            )
        }
    };
    ensure!(
        signer == &demand.admission.request.node_did,
        "file inventory reply is not signed by its source"
    );
    ensure!(
        verifier.verify(signer, &payload, signature).await?,
        "file inventory reply signature is invalid"
    );
    Ok(())
}

pub async fn sign_refusal(
    identity: &dyn NodeIdentity,
    demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
    reason: gents_protocol::file_inventory::FileInventoryRefusalReason,
) -> Result<gents_protocol::file_inventory::FileInventoryRefusalReceipt> {
    use gents_protocol::file_inventory::{
        FileInventoryRefusalReceipt, FILE_INVENTORY_REFUSAL_VERSION,
    };
    ensure!(
        identity.did() == demand.admission.request.node_did,
        "file inventory refusal requires the exact source"
    );
    let mut receipt = FileInventoryRefusalReceipt {
        version: FILE_INVENTORY_REFUSAL_VERSION,
        request_key: demand.derive_request_key()?,
        demand: demand.clone(),
        reason,
        signer_did: identity.did().to_owned(),
        signature: Vec::new(),
    };
    receipt.signature = identity.sign(&receipt.signing_payload()?).await?;
    Ok(receipt)
}

pub(crate) struct InventoryDeliveryObservation {
    pub before: bool,
    pub after: bool,
    pub delivery: crate::agent::p2p_reconcile::session_hydration::HydrationDeliveryResult,
    pub write: crate::agent::p2p_reconcile::session_hydration::HydrationTerminalWriteResult,
}

pub(crate) async fn settle_reply(
    identity: &dyn NodeIdentity,
    demand: &gents_protocol::file_inventory::FileInventoryPageDemand,
    candidate: &gents_protocol::file_inventory::FileInventoryReply,
    observation: InventoryDeliveryObservation,
) -> Result<Option<gents_protocol::file_inventory::FileInventoryReply>> {
    use crate::agent::p2p_reconcile::session_hydration::{
        HydrationDeliveryResult, HydrationTerminalWriteResult,
    };
    use gents_protocol::file_inventory::{FileInventoryRefusalReason, FileInventoryReply};
    if observation.write != HydrationTerminalWriteResult::Committed {
        return Ok(None);
    }
    match candidate {
        FileInventoryReply::Refused(_) => Ok(Some(candidate.clone())),
        FileInventoryReply::Page(_) if !observation.before || !observation.after => {
            Ok(Some(FileInventoryReply::Refused(
                sign_refusal(identity, demand, FileInventoryRefusalReason::Rejected).await?,
            )))
        }
        FileInventoryReply::Page(_)
            if observation.delivery == HydrationDeliveryResult::Indeterminate =>
        {
            Ok(None)
        }
        FileInventoryReply::Page(_) => Ok(Some(candidate.clone())),
    }
}

pub(crate) mod delivery;
pub(crate) mod requests;
pub use delivery::VerifiedInventoryResponse;
pub use requests::admit_demand;

pub(crate) mod journal;
pub use journal::{load_recorded_reply, record_reply};

#[cfg(test)]
mod tests;
