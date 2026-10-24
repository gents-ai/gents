use anyhow::Result;
use rapidhash::{HashMapExt, HashSetExt, RapidHashMap, RapidHashSet};
use serde::{Deserialize, Serialize};

use crate::enrollment::canonical_domain_payload;
use crate::file_changes::{FileChangeImages, FileImage, ImageFailure};
use crate::file_image_hydration::FileImageScope;
use crate::session_hydration::HydrationRequest;

pub const FILE_INVENTORY_PAGE_LIMIT: usize = 64;
pub const FILE_INVENTORY_OPERATOR_PATH: &str = "/file-inventory/operator/page";
pub const FILE_INVENTORY_RECEIPT_VERSION: u8 = 2;
const RECEIPT_DOMAIN: &str = "gents-file-inventory-page-v2";

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryAdmission {
    pub request: HydrationRequest,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileInventoryCursor {
    pub binding: FileInventoryBinding,
    #[cfg_attr(feature = "typescript", ts(type = "number"))]
    pub offset: u64,
}

/// Persist `attempt_id` before posting. Transport retries reuse this demand;
/// an explicit fresh attempt changes its key without rewriting an old receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPageDemand {
    pub admission: FileInventoryAdmission,
    pub attempt_id: String,
    pub cursor: Option<FileInventoryCursor>,
}

impl FileInventoryPageDemand {
    pub fn validate_reference(&self) -> Result<()> {
        let request = &self.admission.request;
        anyhow::ensure!(
            [
                self.attempt_id.as_str(),
                &self.admission.session_doc_id,
                &request.request_key,
                &request.peer_id,
                &request.requester_did,
                &request.node_did,
                &request.session_id,
            ]
            .iter()
            .all(|value| !value.is_empty()),
            "file inventory demand has an empty reference"
        );
        if let Some(cursor) = &self.cursor {
            anyhow::ensure!(
                cursor.binding.admission == self.admission
                    && !cursor.binding.digest.is_empty()
                    && cursor.offset != 0
                    && cursor.offset % FILE_INVENTORY_PAGE_LIMIT as u64 == 0
                    && cursor.offset < cursor.binding.coverage.entries,
                "file inventory demand has an invalid continuation"
            );
        }
        Ok(())
    }

    pub fn derive_request_key(&self) -> Result<String> {
        self.validate_reference()?;
        let value = serde_json::to_string(self)?;
        use sha2::{Digest, Sha256};
        Ok(bs58::encode(Sha256::digest(canonical_domain_payload(
            "gents-file-inventory-demand-v1",
            [value.as_str()],
        )))
        .into_string())
    }

    pub fn validate_page_reference(&self, receipt: &FileInventoryPageReceipt) -> Result<()> {
        anyhow::ensure!(
            receipt.request_key == self.derive_request_key()?
                && receipt.page.binding.admission == self.admission,
            "file inventory receipt does not match its demand"
        );
        match &self.cursor {
            None => anyhow::ensure!(
                receipt.page.offset == 0,
                "initial file inventory demand did not start at zero"
            ),
            Some(cursor) => anyhow::ensure!(
                receipt.page.binding == cursor.binding && receipt.page.offset == cursor.offset,
                "file inventory receipt does not match its continuation"
            ),
        }
        Ok(())
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPageReceipt {
    pub version: u8,
    pub request_key: String,
    pub signer_did: String,
    pub page: FileInventoryPage,
    pub signature: Vec<u8>,
}

impl FileInventoryPageReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let page = serde_json::to_string(&self.page)?;
        Ok(canonical_domain_payload(
            RECEIPT_DOMAIN,
            [version.as_str(), &self.request_key, &self.signer_did, &page],
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum FileInventoryRefusalReason {
    Rejected,
    Stale,
    Unavailable,
}

pub const FILE_INVENTORY_REFUSAL_VERSION: u8 = 1;

/// Refusals attest only the client's own demand and a fixed reason. They
/// contain no selected file membership and do not authorize delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryRefusalReceipt {
    pub version: u8,
    pub request_key: String,
    pub demand: FileInventoryPageDemand,
    pub reason: FileInventoryRefusalReason,
    pub signer_did: String,
    pub signature: Vec<u8>,
}

impl FileInventoryRefusalReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let demand = serde_json::to_string(&self.demand)?;
        let reason = serde_json::to_string(&self.reason)?;
        Ok(canonical_domain_payload(
            "gents-file-inventory-refusal-v1",
            [
                version.as_str(),
                &self.request_key,
                &demand,
                &reason,
                &self.signer_did,
            ],
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "receipt",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum FileInventoryReply {
    Page(FileInventoryPageReceipt),
    Refused(FileInventoryRefusalReceipt),
}

pub const FILE_INVENTORY_PAGE_GRANT_VERSION: u8 = 1;

/// A control-plane receipt binds one metadata document. The metadata itself
/// travels only through explicit document delivery, separate from this pointer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileInventoryPageGrant {
    pub version: u8,
    pub demand: FileInventoryPageDemand,
    pub document_id: String,
    pub signer_did: String,
    pub signature: Vec<u8>,
}

impl FileInventoryPageGrant {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let version = self.version.to_string();
        let demand = serde_json::to_string(&self.demand)?;
        Ok(canonical_domain_payload(
            "gents-file-inventory-page-grant-v1",
            [
                version.as_str(),
                &demand,
                &self.document_id,
                &self.signer_did,
            ],
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "receipt",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum FileInventoryResponse {
    Page(FileInventoryPageGrant),
    Refused(FileInventoryRefusalReceipt),
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
