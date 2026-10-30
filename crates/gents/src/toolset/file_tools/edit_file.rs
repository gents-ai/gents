use super::change_capture;
use std::fmt::Write as _;

use crate::llm::tool::Tool;
use crate::llm::tool::ToolDefinition;
use anyhow::{anyhow, Result};
use serde::Serialize;

use super::super::args::EditFileArgs;
use super::super::edit_match::{self, EditOutcome, EditRequest, MatchMode, Operation};
use super::super::shared::{ToolContext, ToolError};
use super::{
    content_hash, deny_workspace_file_writes, file_mutation_lock_for, merge_optional_notes,
    render_tool_output,
};
use crate::tool_call_lifecycle::FailureClass;

#[derive(Clone)]
pub(in crate::toolset) struct EditFileTool {
    context: ToolContext,
    writethrough: Option<crate::toolset::lsp::LspWritethrough>,
}

impl EditFileTool {
    pub(in crate::toolset) fn new(context: ToolContext) -> Self {
        Self {
            context,
            writethrough: None,
        }
    }

    pub(in crate::toolset) fn with_writethrough(
        mut self,
        writethrough: crate::toolset::lsp::LspWritethrough,
    ) -> Self {
        self.writethrough = Some(writethrough);
        self
    }
}

impl Tool for EditFileTool {
    const NAME: &'static str = "edit_file";

    type Error = ToolError;
    type Args = EditFileArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Edit an existing file under the configured root by replacing old_text with new_text. Relative paths resolve from the active request workspace when one is provided, otherwise from the root. Matching tolerates trailing-whitespace, indentation, and unicode-punctuation drift (the result reports match_strategy; an exact match always wins), and files are matched with normalized line endings, so CRLF files edit cleanly. Use the smallest old_text that uniquely identifies the change; if it matches multiple places the call fails with the locations. On failure the error includes the closest near-match — re-read the file and build new old_text from CURRENT content instead of retrying the same call. Set dry_run=true to preview the diff without writing. Pass expected_content_hash (from read_file) to reject the edit if the file changed since you read it. Set raw_json=true for structured JSON.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Existing file to edit, relative to the active workspace/root unless an allowed absolute path is provided."
                    },
                    "old_text": {
                        "type": "string",
                        "description": "Text to locate. Whitespace/indentation drift is tolerated; interior wording must match. With match_mode=\"regex\" this is a Rust regex instead."
                    },
                    "new_text": {
                        "type": "string",
                        "description": "Replacement text. With match_mode=\"regex\", capture groups substitute ($1, $2, ...)."
                    },
                    "replace_all": {
                        "type": "boolean",
                        "default": false,
                        "description": "When false, old_text must match exactly one location. When true, replace every match."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return the diff that WOULD be applied without writing the file."
                    },
                    "match_mode": {
                        "type": "string",
                        "enum": ["ladder", "regex"],
                        "default": "ladder",
                        "description": "ladder = tolerant literal matching (default). regex = old_text is a Rust regex."
                    },
                    "operation": {
                        "type": "string",
                        "enum": ["replace", "insert_after", "insert_before", "delete"],
                        "default": "replace",
                        "description": "replace swaps old_text for new_text; insert_after/insert_before keep old_text and add new_text after/before it; delete removes old_text (new_text ignored)."
                    },
                    "expected_content_hash": {
                        "type": "string",
                        "description": "Optional content_hash from a prior read_file of this file. If the file's current bytes hash differently, the edit is rejected before matching."
                    },
                    "raw_json": {
                        "type": "boolean",
                        "default": false,
                        "description": "When true, return structured JSON instead of the compact default text."
                    }
                },
                "required": ["path", "old_text", "new_text"]
            }),
        }
    }

    async fn call(&self, args: Self::Args) -> Result<Self::Output, Self::Error> {
        if !args.dry_run {
            deny_workspace_file_writes()?;
        }
        let path = self.context.resolve_existing_file(&args.path)?;
        let consent = if args.dry_run {
            None
        } else {
            crate::plugin::approval::workspace::authorize_file_mutation(
                Self::NAME,
                &self.context.root(),
                &path,
                &args,
            )
            .await
            .map_err(|error| {
                ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
            })?
        };
        let display = self.context.display_path(&path);
        let lock = file_mutation_lock_for(&path);
        let _guard = lock.lock().await;
        if let Some(consent) = consent {
            consent.validate().map_err(|error| {
                ToolError::reported_failure(FailureClass::PolicyDenied, format!("{error:#}"))
            })?;
        }
        let raw = tokio::fs::read(&path).await?;
        let pre_edit_hash = content_hash(&raw);

        if let Some(expected) = &args.expected_content_hash {
            if expected != &pre_edit_hash {
                return Err(anyhow!(
                    "{display} has changed since it was read (expected {expected}). Re-read it and rebuild the edit from its current content."
                )
                .into());
            }
        }

        let operation = match args.operation.as_deref() {
            None | Some("replace") => Operation::Replace,
            Some("insert_after") => Operation::InsertAfter,
            Some("insert_before") => Operation::InsertBefore,
            Some("delete") => Operation::Delete,
            Some(other) => {
                return Err(anyhow!(
                "unknown operation {other:?}; valid: replace, insert_after, insert_before, delete"
            )
                .into())
            }
        };
        let match_mode = match args.match_mode.as_deref() {
            None | Some("ladder") => MatchMode::Ladder,
            Some("regex") => MatchMode::Regex,
            Some(other) => {
                return Err(anyhow!("unknown match_mode {other:?}; valid: ladder, regex").into());
            }
        };

        // A lossy conversion followed by a full-file rewrite would silently
        // replace every invalid byte with U+FFFD.
        let raw_text = String::from_utf8(raw).map_err(|error| {
            anyhow!(
                "{display} is not valid UTF-8 (first invalid byte at offset {}); edit_file only edits UTF-8 text files",
                error.utf8_error().valid_up_to()
            )
        })?;
        let normalized = edit_match::normalize_content(&raw_text);
        let old_text = args.old_text.replace("\r\n", "\n");
        let new_text = args.new_text.replace("\r\n", "\n");
        let request = EditRequest {
            old_text: &old_text,
            new_text: &new_text,
            replace_all: args.replace_all,
            operation,
            match_mode,
        };

        match edit_match::decide(&normalized.text, &request) {
            EditOutcome::Applied {
                result,
                strategy,
                replacements,
                first_changed_line,
                diff,
            } => {
                let post_text =
                    edit_match::restore_content(&result, normalized.ending, normalized.had_bom);
                let post_edit_hash = content_hash(post_text.as_bytes());
                let mut edit_note = None;
                if !args.dry_run {
                    let capture =
                        change_capture::FileChangeCapture::start(&self.context.root(), &path).await;
                    let mutation = async {
                        tokio::fs::write(&path, post_text.as_bytes()).await?;
                        let verify = tokio::fs::read(&path).await?;
                        if verify != post_text.as_bytes() {
                            return Err(anyhow!(
                                "post-write verification failed for {display}: the bytes on disk do not match the edited content"
                            )
                            .into());
                        }
                        if let Some(writethrough) = &self.writethrough {
                            edit_note = writethrough
                                .after_mutation_under_lock(
                                    &path,
                                    crate::toolset::lsp::MutationKind::Edit,
                                )
                                .await;
                        }
                        Ok::<_, ToolError>(())
                    }
                    .await;
                    if let Some(capture) = capture {
                        capture.finish().await;
                    }
                    mutation?;
                    drop(_guard);
                    if let Some(writethrough) = &self.writethrough {
                        let diag = writethrough
                            .diagnostics_after_unlock(
                                &path,
                                crate::toolset::lsp::MutationKind::Edit,
                            )
                            .await;
                        edit_note = merge_optional_notes(edit_note, diag);
                    }
                }
                let output = EditFileOutput {
                    metadata: EditFileMetadata {
                        resolved_path: path.to_string_lossy().into_owned(),
                        ok: true,
                        status: "success",
                        tool: Self::NAME,
                        path: display,
                        returned_count: replacements,
                        total_count: Some(replacements),
                        truncated: false,
                        replacements_applied: replacements,
                        replace_all: args.replace_all,
                        dry_run: args.dry_run,
                        match_strategy: strategy.as_str(),
                        first_changed_line,
                        pre_edit_hash,
                        post_edit_hash,
                        bytes_written: if args.dry_run { 0 } else { post_text.len() },
                    },
                    diff,
                };
                let verb = if args.dry_run {
                    "dry-run preview for"
                } else {
                    "edited"
                };
                let mut body = format!(
                    "edit_file: {verb} {} ({} replacement{}, strategy {})\n{}",
                    output.metadata.path,
                    replacements,
                    if replacements != 1 { "s" } else { "" },
                    output.metadata.match_strategy,
                    output.diff,
                );
                if let Some(note) = edit_note.filter(|note| !note.is_empty()) {
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
            EditOutcome::NotFound { closest } => {
                let mut message = format!("old_text was not found in {display}.");
                if let Some(c) = closest {
                    let _ = write!(
                        message,
                        "\nClosest match ({}% similar) at line {}:",
                        c.similarity_pct, c.line
                    );
                    if let Some((pattern_line, file_line)) = c.first_diff {
                        let _ = write!(
                            message,
                            "\n  your text: {pattern_line}\n  file has:  {file_line}"
                        );
                    }
                }
                message.push_str(
                    "\nRe-read the file and build old_text from its CURRENT content; do not retry the same call.",
                );
                Err(anyhow!(message).into())
            }
            EditOutcome::Ambiguous {
                strategy,
                count,
                previews,
            } => {
                let mut message = format!(
                    "old_text matches {count} locations in {display} (strategy {}):",
                    strategy.as_str()
                );
                for preview in previews {
                    let _ = write!(message, "\n  line {}: {}", preview.line, preview.text);
                }
                message.push_str(
                    "\nAdd surrounding context to make it unique, or set replace_all=true.",
                );
                Err(anyhow!(message).into())
            }
            EditOutcome::Noop { .. } => Err(anyhow!(
                "the edit would produce identical content in {display}; nothing was written. old_text and new_text are equivalent at the matched site."
            )
            .into()),
            EditOutcome::InvalidRegex { error } => Err(anyhow!(
                "match_mode=\"regex\": pattern failed to parse: {error}"
            )
            .into()),
        }
    }

    fn into_dyn_error(error: Self::Error) -> crate::llm::tool::ToolError {
        error.into_dispatch_error()
    }
}

#[derive(Serialize)]
struct EditFileMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    resolved_path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    replacements_applied: usize,
    replace_all: bool,
    dry_run: bool,
    match_strategy: &'static str,
    first_changed_line: usize,
    pre_edit_hash: String,
    post_edit_hash: String,
    bytes_written: usize,
}

#[derive(Serialize)]
struct EditFileOutput {
    #[serde(flatten)]
    metadata: EditFileMetadata,
    diff: String,
}
