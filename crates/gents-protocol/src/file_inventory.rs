use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde::{Deserialize, Serialize};

use crate::file_changes::{FileChangeImages, FileImage, ImageFailure};

pub const FILE_INVENTORY_PAGE_LIMIT: usize = 64;
pub const FILE_INVENTORY_OPERATOR_PATH: &str = "/file-inventory/operator/page";
/// The direct inventory read contract a runtime advertises to its clients.
pub const FILE_INVENTORY_CONTRACT_VERSION: u8 = 2;

/// The origin session scope of a physical tool's capture. Retained images stay
/// bound to it; native document ACP alone decides who may read them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct FileImageScope {
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

/// Metadata is a separate projection: observing it never transfers image bytes
/// or grants a retained-image reader relation. Capture validation stays with
/// the native file-image owner before projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum FileImageMetadata {
    Missing {},
    File { content_hash: String },
    Unavailable { reason: ImageFailure },
}

impl From<&FileImage> for FileImageMetadata {
    fn from(image: &FileImage) -> Self {
        match image {
            FileImage::Missing => Self::Missing {},
            FileImage::File { content_hash, .. } => Self::File {
                content_hash: content_hash.clone(),
            },
            FileImage::Unavailable { reason } => Self::Unavailable {
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileChangeMetadata {
    pub root: String,
    pub path: String,
    pub before: FileImageMetadata,
    pub after: FileImageMetadata,
}

impl From<&FileChangeImages> for FileChangeMetadata {
    fn from(change: &FileChangeImages) -> Self {
        Self {
            root: change.root.clone(),
            path: change.path.clone(),
            before: (&change.before).into(),
            after: (&change.after).into(),
        }
    }
}

/// The authenticated reader and session an inventory read is bound to.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct FileInventoryRequest {
    pub request_key: String,
    pub requester_did: String,
    pub node_did: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryAdmission {
    pub request: FileInventoryRequest,
    pub session_doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryCoverage {
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub entries: u64,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub pending: u64,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub unknown: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryBinding {
    pub admission: FileInventoryAdmission,
    pub digest: String,
    pub coverage: FileInventoryCoverage,
}

/// Chronology derived by the source from selected canonical publications.
/// This establishes neither image continuity, coverage nor read authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryOrdinal {
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub index: u64,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub entries: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryEntry {
    pub tool_call_doc_id: String,
    pub request_doc_id: String,
    pub scope: FileImageScope,
    pub projection_doc_id: String,
    pub observation_doc_id: String,
    pub change: FileChangeMetadata,
    /// Absent observations carry no chronology. Omission from the signing
    /// payload preserves retained receipts without granting an inferred order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub order: Option<FileInventoryOrdinal>,
    /// Source-derived coverage for this physical request in this origin scope.
    /// Omitted claims establish no request-specific completeness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typescript", ts(optional))]
    pub request_coverage: Option<FileInventoryCoverage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryCursor {
    pub binding: FileInventoryBinding,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPage {
    pub binding: FileInventoryBinding,
    pub offset: u64,
    pub entries: Vec<FileInventoryEntry>,
    pub next: Option<FileInventoryCursor>,
}

impl FileInventoryPage {
    pub fn well_formed(&self) -> bool {
        let count = self.entries.len() as u64;
        let Some(remaining) = self.binding.coverage.entries.checked_sub(self.offset) else {
            return false;
        };
        let Some(following) = self.offset.checked_add(count) else {
            return false;
        };
        let next = (following < self.binding.coverage.entries).then(|| FileInventoryCursor {
            binding: self.binding.clone(),
            offset: following,
        });
        !self.binding.digest.is_empty()
            && self.entries.len() <= FILE_INVENTORY_PAGE_LIMIT
            && count == remaining.min(FILE_INVENTORY_PAGE_LIMIT as u64)
            && self.next == next
            && self.entries.iter().all(|entry| {
                entry
                    .order
                    .as_ref()
                    .is_none_or(|order| order.entries <= self.binding.coverage.entries)
                    && entry.request_coverage.as_ref().is_none_or(|coverage| {
                        coverage.entries <= self.binding.coverage.entries
                            && coverage.pending <= self.binding.coverage.pending
                            && coverage.unknown <= self.binding.coverage.unknown
                    })
            })
            && coherent_inventory_entries(&self.entries)
    }
}

/// Origin scopes are retained across forks. Coherence checks representation,
/// not whether an entry belongs to the receiver's authorized session closure.
/// That selection remains with the source's native snapshot owner.
pub fn coherent_inventory_entries(entries: &[FileInventoryEntry]) -> bool {
    let mut keys = RapidHashSet::new();
    let mut tools = RapidHashMap::new();
    let mut projections = RapidHashMap::new();
    let mut observations = RapidHashMap::new();
    let mut orders = RapidHashMap::new();
    let mut positions = RapidHashMap::new();
    let mut requests = RapidHashMap::new();
    let mut previous: Option<&str> = None;
    for entry in entries {
        if [
            entry.tool_call_doc_id.as_str(),
            &entry.request_doc_id,
            &entry.projection_doc_id,
            &entry.observation_doc_id,
            &entry.scope.requester_did,
            &entry.scope.node_did,
            &entry.scope.session_id,
            &entry.change.root,
            &entry.change.path,
        ]
        .iter()
        .any(|value| value.is_empty())
            || entry
                .order
                .as_ref()
                .is_some_and(|order| order.index >= order.entries)
            || entry
                .request_coverage
                .as_ref()
                .is_some_and(|coverage| coverage.entries == 0)
            || previous.is_some_and(|tool| tool > entry.tool_call_doc_id.as_str())
            || !keys.insert((
                &entry.projection_doc_id,
                &entry.change.root,
                &entry.change.path,
            ))
        {
            return false;
        }
        previous = Some(&entry.tool_call_doc_id);
        let request = (
            &entry.scope.node_did,
            &entry.scope.session_id,
            &entry.scope.requester_did,
            &entry.request_doc_id,
        );
        if requests
            .insert(request, entry.request_coverage.as_ref())
            .is_some_and(|prior| prior != entry.request_coverage.as_ref())
        {
            return false;
        }
        let resource = (
            &entry.scope.node_did,
            &entry.change.root,
            &entry.change.path,
        );
        if orders
            .insert(resource, entry.order.as_ref())
            .is_some_and(|prior| match (prior, entry.order.as_ref()) {
                (None, None) => false,
                (Some(left), Some(right)) => left.entries != right.entries,
                _ => true,
            })
        {
            return false;
        }
        if let Some(order) = &entry.order {
            if positions
                .insert((resource, order.index), &entry.tool_call_doc_id)
                .is_some_and(|prior| prior != &entry.tool_call_doc_id)
            {
                return false;
            }
        }
        let tool = (
            &entry.request_doc_id,
            &entry.scope,
            &entry.projection_doc_id,
            &entry.observation_doc_id,
        );
        let projection = (
            &entry.tool_call_doc_id,
            &entry.request_doc_id,
            &entry.scope,
            &entry.observation_doc_id,
        );
        if tools
            .insert(&entry.tool_call_doc_id, tool)
            .is_some_and(|prior| prior != tool)
            || projections
                .insert(&entry.projection_doc_id, projection)
                .is_some_and(|prior| prior != projection)
            || observations
                .insert(&entry.observation_doc_id, &entry.projection_doc_id)
                .is_some_and(|prior| prior != &entry.projection_doc_id)
        {
            return false;
        }
    }
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum FileInventoryRefusalReason {
    Rejected,
    Stale,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryViewCursor {
    pub attempt_id: String,
    pub snapshot_digest: String,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryImageReference {
    pub cursor: FileInventoryViewCursor,
    pub entry_index: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryImage {
    pub reference: FileInventoryImageReference,
    pub entry: FileInventoryEntry,
    pub change: crate::file_changes::FileChangeImages,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum FileInventoryReadAction {
    Read {
        #[serde(default)]
        #[cfg_attr(feature = "typescript", ts(optional = nullable))]
        cursor: Option<FileInventoryViewCursor>,
    },
    Advance {},
    Refresh {
        #[serde(rename = "attemptId")]
        attempt_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirectFileInventoryRequest {
    pub node_did: String,
    pub session_id: String,
    pub action: FileInventoryReadAction,
}

impl Default for FileInventoryReadAction {
    fn default() -> Self {
        Self::Read { cursor: None }
    }
}

/// One retained source page, with completeness derived from the full verified
/// prefix. `next` addresses only pages already received by this client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventorySnapshotView {
    pub attempt_id: String,
    pub binding: FileInventoryBinding,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub received_entries: u64,
    pub complete: bool,
    pub known_inventory: bool,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub offset: u64,
    pub entries: Vec<FileInventoryEntry>,
    pub next: Option<FileInventoryViewCursor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryObservation {
    pub admission: FileInventoryAdmission,
    pub attempt_id: String,
    pub current: Option<FileInventorySnapshotView>,
    pub previous: Option<FileInventorySnapshotView>,
    pub refusal: Option<FileInventoryRefusalReason>,
}
