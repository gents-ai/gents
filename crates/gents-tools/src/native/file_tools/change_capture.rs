use std::fs::Metadata;
use std::io::Read as _;

use base64::Engine as _;
use gents_loop::live_output::{DeferredToolWrites, LiveToolOutputWriter};
use gents_protocol::file_changes::{
    FileChangeImages as FileChange, FileImage, FilePermissions, ImageFailure,
    ToolFileChanges as ChangeBatch, FILE_CHANGES_VERSION, MAX_FILE_IMAGE_BYTES as MAX_IMAGE_BYTES,
};

use gents_fs_runner::beneath::{Access, BeneathError, Located};
use gents_session::file_contents::{content_hash, FileChanges, VerifiedUnchanged};

pub(super) fn unchanged_output() -> DeferredToolWrites {
    DeferredToolWrites::new(VerifiedUnchanged::witness())
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

/// The image of a located file, read through the handle it was resolved to.
pub fn read_image(target: &Located) -> FileImage {
    let file = match target.open(Access::Read) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return FileImage::Missing,
        Err(BeneathError::NotAFile { .. } | BeneathError::Changed { .. }) => {
            return FileImage::Unavailable {
                reason: ImageFailure::UnsupportedEntry,
            }
        }
        Err(_) => {
            return FileImage::Unavailable {
                reason: ImageFailure::ReadFailed,
            }
        }
    };
    read_open_image(target, file).unwrap_or(FileImage::Unavailable {
        reason: ImageFailure::ReadFailed,
    })
}

fn path_matches_open_image(target: &Located, file: &std::fs::File) -> std::io::Result<bool> {
    let opened = same_file::Handle::from_file(file.try_clone()?)?;
    let named = target.open(Access::Read).map_err(std::io::Error::other)?;
    Ok(opened == same_file::Handle::from_file(named)?)
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
fn read_open_image(target: &Located, file: std::fs::File) -> std::io::Result<FileImage> {
    let metadata = file.metadata()?;
    if !path_matches_open_image(target, &file).unwrap_or(false) {
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
        || !path_matches_open_image(target, reader.get_ref()).unwrap_or(false)
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

async fn image(target: Located) -> FileImage {
    tokio::task::spawn_blocking(move || read_image(&target))
        .await
        .unwrap_or(FileImage::Unavailable {
            reason: ImageFailure::ReadFailed,
        })
}

pub struct FileChangeCapture {
    writer: LiveToolOutputWriter,
    root: String,
    relative: String,
    target: Located,
    before: FileImage,
}

impl FileChangeCapture {
    /// The native mutator has authorized and resolved this path and holds its
    /// existing mutation lock. A pending image survives cancellation after a
    /// partial write; it cannot be mistaken for a verified postimage.
    pub async fn start(
        call: &gents_loop::tool::ToolCallEnvelope,
        target: &Located,
    ) -> Option<Self> {
        let writer = call.runtime()?.live_output?;
        let root = target.root().path();
        let relative = target
            .relative()
            .to_str()?
            .replace(std::path::MAIN_SEPARATOR, "/");
        gents_session::workspace::WorkspacePathCapability::exact_paths(vec![relative.clone()])
            .ok()?;
        let capture = Self {
            writer,
            root: root.to_str()?.to_owned(),
            relative,
            target: target.clone(),
            before: image(target.clone()).await,
        };
        capture
            .record(FileImage::Unavailable {
                reason: ImageFailure::MutationIncomplete,
            })
            .await;
        Some(capture)
    }

    pub(super) async fn finish(self) {
        let after = image(self.target.clone()).await;
        self.record(after).await;
    }

    async fn record(&self, after: FileImage) {
        let changes = FileChanges::new(ChangeBatch {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: self.writer.tool_call_doc_id().to_owned(),
            changes: vec![FileChange {
                root: self.root.clone(),
                path: self.relative.clone(),
                before: self.before.clone(),
                after,
            }],
        });
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
