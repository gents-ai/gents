use std::sync::Arc;

use base64::Engine as _;
use gents_protocol::file_changes::{
    FileImage, ToolFileChanges as ChangeBatch, FILE_CHANGES_VERSION,
    MAX_FILE_IMAGE_BYTES as MAX_IMAGE_BYTES,
};
use rapidhash::{HashSetExt, RapidHashSet};
use serde::{Serialize, Serializer};

use super::content_hash;

/// Only native file operations whose implementation cannot mutate files emit
/// this witness. Dispatch binds it to the accepted physical invocation; its
/// winning terminal supplies request and source scope. Success alone is not a
/// witness for shell, plugin or other arbitrary operations.
#[derive(Debug, Clone)]
pub struct VerifiedUnchanged(());

impl VerifiedUnchanged {
    pub(crate) fn witness() -> Self {
        Self(())
    }
}

#[derive(Clone)]
pub struct FileChanges(Arc<ChangeBatch>);

impl Serialize for FileChanges {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

impl FileChanges {
    pub(crate) fn new(batch: ChangeBatch) -> Self {
        Self(Arc::new(batch))
    }

    pub(crate) fn metadata(&self) -> Vec<gents_protocol::file_inventory::FileChangeMetadata> {
        self.0.changes.iter().map(Into::into).collect()
    }

    pub(crate) fn batch(&self) -> &ChangeBatch {
        &self.0
    }

    pub(crate) fn belongs_to(&self, tool_call_doc_id: &str) -> bool {
        self.0.tool_call_doc_id == tool_call_doc_id
    }

    pub(crate) fn decode(
        tool_call_doc_id: &str,
        value: serde_json::Value,
    ) -> anyhow::Result<ChangeBatch> {
        let changes: ChangeBatch = serde_json::from_value(value)?;
        anyhow::ensure!(
            changes.version == FILE_CHANGES_VERSION
                && !tool_call_doc_id.is_empty()
                && changes.tool_call_doc_id == tool_call_doc_id
                && !changes.changes.is_empty(),
            "invalid retained file change binding"
        );
        let mut paths = RapidHashSet::new();
        for change in &changes.changes {
            anyhow::ensure!(
                !change.root.is_empty() && paths.insert((&change.root, &change.path)),
                "ambiguous retained file path"
            );
            crate::workspace::WorkspacePathCapability::exact_paths(vec![change.path.clone()])?;
            for image in [&change.before, &change.after] {
                if let FileImage::File {
                    content_hash: expected,
                    data,
                    permissions,
                    links,
                } = image
                {
                    anyhow::ensure!(
                        data.len() as u64 <= MAX_IMAGE_BYTES.div_ceil(3) * 4
                            && permissions.unix_mode.is_none_or(|mode| mode <= 0o7777)
                            && links.is_none_or(|count| count > 0),
                        "invalid retained file image"
                    );
                    let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
                    anyhow::ensure!(
                        bytes.len() as u64 <= MAX_IMAGE_BYTES && content_hash(&bytes) == *expected,
                        "retained file image does not match its content hash"
                    );
                }
            }
        }
        Ok(changes)
    }
}
