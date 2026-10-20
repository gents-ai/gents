use serde::{Deserialize, Serialize};

use super::session_hydration::{
    decide_hydration, HydrationCatalog, HydrationRequest, HydrationVerdict,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageDemand {
    pub admission: HydrationRequest,
    pub request_doc_id: String,
    pub tool_call_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FileImageScope {
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

impl FileImageScope {
    fn matches(&self, request: &HydrationRequest) -> bool {
        self.requester_did == request.requester_did
            && self.node_did == request.node_did
            && self.session_id == request.session_id
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum FileImageSelection {
    Rejected,
    Missing,
    Selected { document: String },
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
                && image.scope.matches(request) =>
        {
            Selected {
                document: image.doc_id.clone(),
            }
        }
        _ => Rejected,
    }
}

#[cfg(test)]
mod tests;
