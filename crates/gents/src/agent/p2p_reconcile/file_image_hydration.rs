use serde::{Deserialize, Serialize};

use super::session_hydration::{decide_hydration, HydrationCatalog, HydrationVerdict};

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

/// A file document whose versions hold an observation's retained bytes. It
/// shares the session scope of every observation that references it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageContent {
    pub doc_id: String,
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
                && image.scope.matches(request)
                && image.contents.iter().all(|content| {
                    !content.doc_id.is_empty() && content.scope.matches(request)
                }) =>
        {
            Selected {
                document: image.doc_id.clone(),
            }
        }
        _ => Rejected,
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
