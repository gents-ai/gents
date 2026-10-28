use serde::{Deserialize, Serialize};

use super::session_hydration::{
    decide_hydration, decide_inventory_hydration, HydrationCatalog, HydrationVerdict,
};
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryEntry, FileInventoryPageReceipt,
    FILE_INVENTORY_RECEIPT_VERSION,
};

pub use gents_protocol::file_image_hydration::{
    FileImageDemand, FileImageScope, FileImageSelection,
};

/// Acceptance and lifecycle observations must come from the physical tool's
/// canonical admission reader, not from a client's demand document.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageTool {
    pub doc_id: String,
    pub request_doc_id: String,
    pub scope: FileImageScope,
    pub accepted: bool,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageObservation {
    pub doc_id: String,
    pub tool_call_doc_id: String,
    pub request_doc_id: String,
    pub scope: FileImageScope,
}

/// Admission retains the session hydration owner. Selected bytes still require
/// the file-image reader's integrity checks and a current enrollment fence at
/// delivery. This decision does not install a collection replicator.
pub fn select_file_images(
    demand: &FileImageDemand,
    catalog: &HydrationCatalog,
    tools: &[FileImageTool],
    images: &[FileImageObservation],
) -> FileImageSelection {
    use FileImageSelection::{Missing, Rejected, Selected};
    if demand.inventory_reference.is_some() {
        return Rejected;
    }
    let request = &demand.admission;
    if [
        &request.request_key,
        &request.peer_id,
        &request.requester_did,
        &request.node_did,
        &request.session_id,
        &demand.request_doc_id,
        &demand.tool_call_doc_id,
    ]
    .iter()
    .any(|value| value.is_empty())
        || matches!(
            decide_hydration(request, catalog),
            HydrationVerdict::Reject(_)
        )
    {
        return Rejected;
    }
    let mut tools = tools
        .iter()
        .filter(|tool| tool.doc_id == demand.tool_call_doc_id);
    let (Some(tool), None) = (tools.next(), tools.next()) else {
        return Rejected;
    };
    if !tool.accepted
        || !tool.terminal
        || tool.request_doc_id != demand.request_doc_id
        || !tool.scope.matches(request)
    {
        return Rejected;
    }
    let mut images = images
        .iter()
        .filter(|image| image.tool_call_doc_id == demand.tool_call_doc_id);
    match (images.next(), images.next()) {
        (None, None) => Missing,
        (Some(image), None)
            if !image.doc_id.is_empty()
                && image.request_doc_id == demand.request_doc_id
                && image.scope.matches(request) =>
        {
            Selected {
                document: image.doc_id.clone(),
            }
        }
        _ => Rejected,
    }
}

/// The source's existing signature and document-owner readers establish these
/// facts. They are not decoded from the client's image demand.
pub(crate) struct InventoryImagePageObservation {
    pub receipt: FileInventoryPageReceipt,
    pub owner: String,
    pub signature_verified: bool,
}

/// Native ACP observes the reviewing DID at the exact physical source root.
/// A readable metadata projection does not establish image visibility.
pub(crate) struct InventoryImageReadObservation {
    pub admission: FileInventoryAdmission,
    pub projection_doc_id: String,
    pub observation_doc_id: String,
    pub projection_visible: bool,
    pub image_visible: bool,
    pub projection_owner: String,
    pub image_owner: String,
}

pub(crate) fn inventory_image_entry<'a>(
    demand: &FileImageDemand,
    page: &'a InventoryImagePageObservation,
) -> Option<&'a FileInventoryEntry> {
    let Some(reference) = &demand.inventory_reference else {
        return None;
    };
    if demand.validate_reference().is_err()
        || !page.signature_verified
        || page.receipt.version != FILE_INVENTORY_RECEIPT_VERSION
        || page.receipt.signer_did != demand.admission.node_did
        || page.owner != page.receipt.signer_did
        || reference
            .page
            .validate_page_reference(&page.receipt)
            .is_err()
        || !page.receipt.page.well_formed()
        || reference.snapshot_digest != page.receipt.page.binding.digest
    {
        return None;
    }
    page.receipt
        .page
        .entries
        .get(usize::from(reference.entry_index))
}

pub(crate) fn authorize_inventory_image_read<'a>(
    demand: &FileImageDemand,
    catalog: &HydrationCatalog,
    page: &'a InventoryImagePageObservation,
    read: &InventoryImageReadObservation,
) -> Option<&'a FileInventoryEntry> {
    let reference = demand.inventory_reference.as_ref()?;
    let entry = inventory_image_entry(demand, page)?;
    if matches!(
        decide_inventory_hydration(&reference.page.admission, catalog),
        HydrationVerdict::Reject(_)
    ) {
        return None;
    }
    let root = &read.admission.request;
    if read.admission.session_doc_id != reference.page.admission.session_doc_id
        || root.peer_id != demand.admission.peer_id
        || root.requester_did != demand.admission.requester_did
        || root.node_did != demand.admission.node_did
        || root.session_id != demand.admission.session_id
        || read.projection_doc_id != entry.projection_doc_id
        || read.observation_doc_id != entry.observation_doc_id
        || !read.projection_visible
        || !read.image_visible
        || read.projection_owner != entry.scope.node_did
        || read.image_owner != entry.scope.node_did
        || entry.scope.node_did != demand.admission.node_did
        || entry.tool_call_doc_id != demand.tool_call_doc_id
        || entry.request_doc_id != demand.request_doc_id
    {
        return None;
    }
    Some(entry)
}

pub(crate) fn select_inventory_file_images(
    demand: &FileImageDemand,
    catalog: &HydrationCatalog,
    page: &InventoryImagePageObservation,
    read: &InventoryImageReadObservation,
    tools: &[FileImageTool],
    images: &[FileImageObservation],
) -> FileImageSelection {
    use FileImageSelection::{Rejected, Selected};
    let Some(entry) = authorize_inventory_image_read(demand, catalog, page, read) else {
        return Rejected;
    };
    let mut tools = tools
        .iter()
        .filter(|tool| tool.doc_id == demand.tool_call_doc_id);
    let mut images = images
        .iter()
        .filter(|image| image.tool_call_doc_id == demand.tool_call_doc_id);
    let (Some(tool), None, Some(image), None) =
        (tools.next(), tools.next(), images.next(), images.next())
    else {
        return Rejected;
    };
    if !tool.accepted
        || !tool.terminal
        || tool.scope != entry.scope
        || tool.request_doc_id != demand.request_doc_id
        || image.scope != entry.scope
        || image.doc_id != entry.observation_doc_id
        || image.tool_call_doc_id != tool.doc_id
        || image.request_doc_id != tool.request_doc_id
    {
        return Rejected;
    }
    Selected {
        document: image.doc_id.clone(),
    }
}

pub fn image_delivery_target(selection: &FileImageSelection, authorized: bool) -> Option<&str> {
    if !authorized {
        return None;
    }
    match selection {
        FileImageSelection::Selected { document } => Some(document),
        _ => None,
    }
}

pub fn image_attempt_delivery_target<'a>(
    prior: Option<&FileImageSelection>,
    selection: &'a FileImageSelection,
    authorized: bool,
) -> Option<&'a str> {
    if prior.is_some() {
        None
    } else {
        image_delivery_target(selection, authorized)
    }
}

pub fn record_image_outcome(
    prior: Option<&FileImageSelection>,
    candidate: &FileImageSelection,
    write: super::session_hydration::HydrationTerminalWriteResult,
) -> Option<FileImageSelection> {
    prior.cloned().or_else(|| {
        (write == super::session_hydration::HydrationTerminalWriteResult::Committed)
            .then(|| candidate.clone())
    })
}

pub fn completed_image_selection(
    selection: &FileImageSelection,
    before_delivery: bool,
    after_delivery: bool,
    delivery: super::session_hydration::HydrationDeliveryResult,
    write: super::session_hydration::HydrationTerminalWriteResult,
) -> Option<FileImageSelection> {
    use super::session_hydration::{HydrationDeliveryResult, HydrationTerminalWriteResult};
    if write != HydrationTerminalWriteResult::Committed {
        return None;
    }
    if !before_delivery || matches!(selection, FileImageSelection::Rejected) {
        return Some(FileImageSelection::Rejected);
    }
    if delivery == HydrationDeliveryResult::Indeterminate {
        return None;
    }
    if !after_delivery {
        return Some(FileImageSelection::Rejected);
    }
    Some(selection.clone())
}

#[cfg(test)]
mod tests;
