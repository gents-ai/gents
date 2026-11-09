use serde::{Deserialize, Serialize};

#[cfg(test)]
use super::admission::{decide_inventory_admission, InventoryCatalog, InventoryVerdict};
#[cfg(test)]
use gents_protocol::file_image_hydration::FileImageDemand;
pub use gents_protocol::file_image_hydration::{FileImageScope, FileImageSelection};
#[cfg(test)]
use gents_protocol::file_inventory::{
    FileInventoryAdmission, FileInventoryEntry, FileInventoryPageReceipt,
    FILE_INVENTORY_RECEIPT_VERSION,
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

/// A file document whose versions hold an observation's retained bytes. It
/// shares the session scope of every observation that references it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageContent {
    pub doc_id: String,
    pub tool_call_doc_id: String,
    pub scope: FileImageScope,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageObservation {
    pub doc_id: String,
    pub tool_call_doc_id: String,
    pub request_doc_id: String,
    pub scope: FileImageScope,
    pub contents: Vec<FileImageContent>,
}

impl FileImageObservation {
    /// Delivery and the read grant cover the observation and its file documents.
    pub fn delivery_documents(&self) -> Vec<String> {
        std::iter::once(self.doc_id.clone())
            .chain(self.contents.iter().map(|content| content.doc_id.clone()))
            .collect()
    }
}

/// The source's existing signature and document-owner readers establish these
/// facts. They are not decoded from the client's image demand.
#[cfg(test)]
pub(crate) struct InventoryImagePageObservation {
    pub receipt: FileInventoryPageReceipt,
    pub owner: String,
    pub signature_verified: bool,
}

/// Native ACP observes the reviewing DID at the exact physical source root.
/// A readable metadata projection does not establish image visibility.
#[cfg(test)]
pub(crate) struct InventoryImageReadObservation {
    pub admission: FileInventoryAdmission,
    pub projection_doc_id: String,
    pub observation_doc_id: String,
    pub projection_visible: bool,
    pub image_visible: bool,
    pub projection_owner: String,
    pub image_owner: String,
}

#[cfg(test)]
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

#[cfg(test)]
pub(crate) fn authorize_inventory_image_read<'a>(
    demand: &FileImageDemand,
    catalog: &InventoryCatalog,
    page: &'a InventoryImagePageObservation,
    read: &InventoryImageReadObservation,
) -> Option<&'a FileInventoryEntry> {
    let reference = demand.inventory_reference.as_ref()?;
    let entry = inventory_image_entry(demand, page)?;
    if matches!(
        decide_inventory_admission(&reference.page.admission, catalog),
        InventoryVerdict::Reject(_)
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

#[cfg(test)]
pub(crate) fn select_inventory_file_images(
    demand: &FileImageDemand,
    catalog: &InventoryCatalog,
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
        || image.contents.iter().any(|content| {
            content.doc_id.is_empty()
                || content.tool_call_doc_id != tool.doc_id
                || content.scope != entry.scope
        })
    {
        return Rejected;
    }
    Selected {
        document: image.doc_id.clone(),
    }
}

#[cfg(test)]
mod tests;
