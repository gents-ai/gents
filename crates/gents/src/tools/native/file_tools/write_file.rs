use super::change_capture;
use std::path::Path;

use crate::inference::llm::tool::Tool;
use crate::inference::llm::tool::ToolDefinition;
use anyhow::{anyhow, Result};
use serde::Serialize;

use super::super::args::WriteFileArgs;
use super::super::shared::{ToolContext, ToolError};
use super::{
    content_hash, deny_workspace_file_writes, file_mutation_lock_for, merge_optional_notes,
    render_tool_output,
};
use crate::tools::call::lifecycle::FailureClass;

/// Stale-content guard for write_file, checked under the per-path mutation
/// lock. No refusal discloses the current hash, so a retry cannot replace
/// changed content without re-reading.
async fn guard_existing_file_overwrite(
    path: &Path,
    display: &str,
    created: bool,
    expected: Option<&str>,
    overwrite: bool,
) -> Result<(), ToolError> {
    if expected.is_some() && overwrite {
        return Err(anyhow!(
            "expected_content_hash and overwrite=true conflict: pass the hash to replace only the content you read, or overwrite=true to replace it regardless, not both."
        )
        .into());
    }
    if created {
        if let Some(expected) = expected {
            return Err(anyhow!(
                "{display} has changed since it was read: expected {expected}, but the file no longer exists. Re-read the directory, or omit expected_content_hash to create a new file."
            )
            .into());
        }
        return Ok(());
    }
    if let Some(expected) = expected {
        let current = content_hash(&tokio::fs::read(path).await?);
        if expected != current {
            return Err(anyhow!(
                "{display} has changed since it was read (expected {expected}). Re-read it and rebuild the write from its current content."
            )
            .into());
        }
        return Ok(());
    }
    if overwrite {
        return Ok(());
    }
    Err(anyhow!(
        "{display} already exists and write_file will not replace it blindly. read_file it and pass its content_hash as expected_content_hash (write_file and edit_file results also report the latest hash), use edit_file for a targeted change, or set overwrite=true to replace it regardless of its current content."
    )
    .into())
}
#[derive(Clone)]
pub(in crate::tools::native) struct WriteFileTool {
    context: ToolContext,
    writethrough: Option<crate::tools::native::lsp::LspWritethrough>,
}

impl WriteFileTool {
    pub(in crate::tools::native) fn new(context: ToolContext) -> Self {
        Self {
            context,
            writethrough: None,
        }
    }

    pub(in crate::tools::native) fn with_writethrough(
        mut self,
        writethrough: crate::tools::native::lsp::LspWritethrough,
    ) -> Self {
        self.writethrough = Some(writethrough);
        self
    }
}

impl Tool for WriteFileTool {
    const NAME: &'static str = "write_file";

    type Error = ToolError;
    type Args = WriteFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Write full file contents under the configured root. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Creating a new file needs no guard. Replacing an existing file requires expected_content_hash (the content_hash from your latest read_file, write_file, or edit_file of that file) and is rejected if the file changed since then; set overwrite=true only to replace it regardless of its current content. Prefer edit_file for targeted changes. Returns compact success metadata, including the written content_hash. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File to create or overwrite, relative to the active workspace/root unless an allowed absolute path is provided."
                    },
                    "content": {
                        "type": "string",
                        "description": "Complete file contents to write. Existing file contents are replaced."
                    },
                    "expected_content_hash": {
                        "type": "string",
                        "description": "content_hash from your latest read_file (or write_file/edit_file result) of this file. Required to replace an existing file unless overwrite=true; if the file's current bytes hash differently, the write is rejected."
                    },
                    "overwrite": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, replace an existing file without checking whether it changed since you last saw it. Do not combine with expected_content_hash."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        deny_workspace_file_writes()?;
        let path = self.context.resolve_path_allow_create(&args.path)?;
        let consent = crate::plugin::approval::workspace::authorize_file_mutation(
            Self::NAME,
            &self.context.root(),
            &path,
            &args,
        )
        .await
        .map_err(|error| {
            ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
        })?;
        let lock = file_mutation_lock_for(&path);
        let _guard = lock.lock().await;
        if let Some(consent) = consent {
            consent.validate().map_err(|error| {
                ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
            })?;
        }
        let created = !path.exists();
        guard_existing_file_overwrite(
            &path,
            &self.context.display_path(&path),
            created,
            args.expected_content_hash.as_deref(),
            args.overwrite,
        )
        .await?;
        let capture = change_capture::FileChangeCapture::start(&self.context.root(), &path).await;
        let mutation = async {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            tokio::fs::write(&path, args.content.as_bytes()).await?;
            let mut bytes_written = args.content.len();
            let mut written_hash = content_hash(args.content.as_bytes());
            let mut note = None;
            if let Some(writethrough) = &self.writethrough {
                note = writethrough
                    .after_mutation_under_lock(
                        &path,
                        crate::tools::native::lsp::MutationKind::Write,
                    )
                    .await;
                if let Ok(bytes) = tokio::fs::read(&path).await {
                    bytes_written = bytes.len();
                    written_hash = content_hash(&bytes);
                }
            }
            Ok::<_, ToolError>((bytes_written, written_hash, note))
        }
        .await;
        if let Some(capture) = capture {
            capture.finish().await;
        }
        let (bytes_written, written_hash, mut note) = mutation?;
        drop(_guard);
        if let Some(writethrough) = &self.writethrough {
            let diag = writethrough
                .diagnostics_after_unlock(&path, crate::tools::native::lsp::MutationKind::Write)
                .await;
            note = merge_optional_notes(note, diag);
        }

        let output = WriteFileOutput {
            metadata: WriteFileMetadata {
                resolved_path: path.to_string_lossy().into_owned(),
                ok: true,
                status: "success",
                tool: Self::NAME,
                path: self.context.display_path(&path),
                returned_count: 0,
                total_count: Some(0),
                truncated: false,
                bytes_written,
                created,
                content_hash: written_hash,
            },
        };

        let mut body = format!(
            "write_file: wrote {} bytes to {}",
            output.metadata.bytes_written, output.metadata.path
        );
        if let Some(note) = note.filter(|note| !note.is_empty()) {
            body.push('\n');
            body.push_str(&note);
        }

        Ok(render_tool_output(
            &output.metadata,
            body,
            &output,
            args.raw_json,
        )?)
    }

    fn into_dyn_error(error: Self::Error) -> crate::inference::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

#[derive(Serialize)]
struct WriteFileMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    resolved_path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    bytes_written: usize,
    created: bool,
    content_hash: String,
}

#[derive(Serialize)]
struct WriteFileOutput {
    #[serde(flatten)]
    metadata: WriteFileMetadata,
}
