use std::collections::HashSet;
use std::fs::Metadata;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use gents_loop::live_output::{DeferredToolWrites, LiveToolOutputWriter};
use gents_protocol::file_changes::{
    FileChangeImages as FileChange, FileImage, FilePermissions, ImageFailure,
    ToolFileChanges as ChangeBatch, FILE_CHANGES_VERSION, MAX_FILE_IMAGE_BYTES as MAX_IMAGE_BYTES,
};
use serde::{Serialize, Serializer};

use super::{content_hash, open_regular_file};

#[derive(Clone)]
pub(crate) struct FileChanges(Arc<ChangeBatch>);

impl Serialize for FileChanges {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.as_ref().serialize(serializer)
    }
}

impl FileChanges {
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
        let mut paths = HashSet::new();
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

fn file_metadata(metadata: &Metadata) -> (FilePermissions, Option<u64>) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            FilePermissions {
                readonly: metadata.permissions().readonly(),
                unix_mode: Some(metadata.mode() & 0o7777),
            },
            Some(metadata.nlink()),
        )
    }
    #[cfg(not(unix))]
    {
        (
            FilePermissions {
                readonly: metadata.permissions().readonly(),
                unix_mode: None,
            },
            None,
        )
    }
}

fn read_image(path: &Path) -> FileImage {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return FileImage::Missing,
        Err(_) => {
            return FileImage::Unavailable {
                reason: ImageFailure::ReadFailed,
            }
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return FileImage::Unavailable {
            reason: ImageFailure::UnsupportedEntry,
        };
    }
    open_regular_file(path)
        .and_then(|file| read_open_image(path, file))
        .unwrap_or(FileImage::Unavailable {
            reason: ImageFailure::ReadFailed,
        })
}

fn path_matches_open_image(path: &Path, file: &std::fs::File) -> std::io::Result<bool> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    let named = same_file::Handle::from_file(open_regular_file(path)?)?;
    Ok(opened == named)
}

fn image_metadata_unchanged(before: &Metadata, after: &Metadata) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec()) {
            return Ok(false);
        }
    }
    Ok(before.len() == after.len()
        && before.modified()? == after.modified()?
        && file_metadata(before) == file_metadata(after))
}

/// A retained descriptor can outlive its directory entry. Both boundary checks
/// must still resolve to that descriptor; timestamps alone do not detect a
/// rename-and-replacement. External writers remain outside the mutation lock.
fn read_open_image(path: &Path, file: std::fs::File) -> std::io::Result<FileImage> {
    let metadata = file.metadata()?;
    if !path_matches_open_image(path, &file).unwrap_or(false) {
        return Ok(FileImage::Unavailable {
            reason: ImageFailure::ChangedDuringRead,
        });
    }
    if metadata.len() > MAX_IMAGE_BYTES {
        return Ok(FileImage::Unavailable {
            reason: ImageFailure::TooLarge,
        });
    }
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_IMAGE_BYTES + 1);
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Ok(FileImage::Unavailable {
            reason: ImageFailure::TooLarge,
        });
    }
    let observed = reader.get_ref().metadata()?;
    if observed.len() != bytes.len() as u64
        || !image_metadata_unchanged(&metadata, &observed)?
        || !path_matches_open_image(path, reader.get_ref()).unwrap_or(false)
    {
        return Ok(FileImage::Unavailable {
            reason: ImageFailure::ChangedDuringRead,
        });
    }
    let (permissions, links) = file_metadata(&metadata);
    Ok(FileImage::File {
        content_hash: content_hash(&bytes),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        permissions,
        links,
    })
}

async fn image(path: PathBuf) -> FileImage {
    tokio::task::spawn_blocking(move || read_image(&path))
        .await
        .unwrap_or(FileImage::Unavailable {
            reason: ImageFailure::ReadFailed,
        })
}

pub(super) struct FileChangeCapture {
    writer: LiveToolOutputWriter,
    root: String,
    relative: String,
    path: PathBuf,
    before: FileImage,
}

impl FileChangeCapture {
    /// The native mutator has authorized and resolved this path and holds its
    /// existing mutation lock. A pending image survives cancellation after a
    /// partial write; it cannot be mistaken for a verified postimage.
    pub(super) async fn start(root: &Path, path: &Path) -> Option<Self> {
        let writer =
            gents_loop::tool_call_lifecycle::runtime::current_tool_runtime_context()?.live_output?;
        let relative = path
            .strip_prefix(root)
            .ok()?
            .to_str()?
            .replace(std::path::MAIN_SEPARATOR, "/");
        crate::workspace::WorkspacePathCapability::exact_paths(vec![relative.clone()]).ok()?;
        let capture = Self {
            writer,
            root: root.to_str()?.to_owned(),
            relative,
            path: path.to_owned(),
            before: image(path.to_owned()).await,
        };
        capture
            .record(FileImage::Unavailable {
                reason: ImageFailure::MutationIncomplete,
            })
            .await;
        Some(capture)
    }

    pub(super) async fn finish(self) {
        let after = image(self.path.clone()).await;
        self.record(after).await;
    }

    async fn record(&self, after: FileImage) {
        let changes = FileChanges(Arc::new(ChangeBatch {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: self.writer.tool_call_doc_id().to_owned(),
            changes: vec![FileChange {
                root: self.root.clone(),
                path: self.relative.clone(),
                before: self.before.clone(),
                after,
            }],
        }));
        if let Err(error) = self
            .writer
            .record_deferred_writes(DeferredToolWrites::new(changes))
            .await
        {
            tracing::warn!(%error, "file change observation could not be retained");
        }
    }
}

#[cfg(test)]
mod tests;
