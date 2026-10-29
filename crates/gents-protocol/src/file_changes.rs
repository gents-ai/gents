use serde::{Deserialize, Serialize};

/// Retention is bounded independently of the bytes a native mutator may edit.
/// An unavailable image prevents restoration without preventing the mutation.
pub const MAX_FILE_IMAGE_BYTES: u64 = 1024 * 1024;
pub const FILE_CHANGES_VERSION: u32 = 1;

/// Immutable host observations bound to an accepted physical tool. Storage
/// commits them beside its winning terminal without changing the tool's schema
/// lineage. Retained bytes are never provider-facing tool output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ToolFileChanges {
    pub version: u32,
    pub tool_call_doc_id: String,
    pub changes: Vec<FileChangeImages>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FileChangeImages {
    /// Resolved tool root at mutation time. Placement and request-resource
    /// checks remain with their existing owners; this path is an observation.
    pub root: String,
    pub path: String,
    pub before: FileImage,
    pub after: FileImage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum FileImage {
    Missing,
    File {
        content_hash: String,
        data: String,
        permissions: FilePermissions,
        links: Option<u64>,
    },
    Unavailable {
        reason: ImageFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub enum ImageFailure {
    ReadFailed,
    ChangedDuringRead,
    TooLarge,
    UnsupportedEntry,
    MutationIncomplete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FilePermissions {
    pub readonly: bool,
    pub unix_mode: Option<u32>,
}

/// Stored observations whose bytes live as versions of per-session file
/// documents rather than inline. Readers resolve them into [`ToolFileChanges`];
/// version 1 rows keep their inline images.
pub const STORED_FILE_CHANGES_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFileChanges {
    pub version: u32,
    pub tool_call_doc_id: String,
    pub changes: Vec<StoredFileChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFileChange {
    pub root: String,
    pub path: String,
    /// The `ToolFileContent` document keyed by this change's session, root and
    /// path. Absent only when neither side retained bytes.
    pub content_doc_id: Option<String>,
    pub before: StoredFileImage,
    pub after: StoredFileImage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub enum StoredFileImage {
    Missing,
    File {
        content_hash: String,
        /// Composite commit CID of the content document holding these bytes.
        version: String,
        encoding: FileEncoding,
        permissions: FilePermissions,
        links: Option<u64>,
    },
    Unavailable {
        reason: ImageFailure,
    },
}

/// UTF-8 bytes live in the content document's Text CRDT field; any other bytes
/// in its Blob field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileEncoding {
    Text,
    Blob,
}
