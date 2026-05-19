use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use glob::Pattern;

use crate::context::RunnerContext;
use crate::output::{
    default_ignored_names, format_entries, format_grep_matches, render_tool_output,
    summarize_entries, total_count, truncate_grep_preview, GlobMetadata, GlobOutput, GrepMetadata,
    GrepOutput, GrepOutputMatch, ListFilesMetadata, ListFilesOutput,
};
use crate::protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};
use crate::traversal::{collect_entries, collect_glob_matches, collect_grep_matches};

pub fn execute_request(root: PathBuf, request: NativeFsRunnerRequest) -> Result<String> {
    let context = RunnerContext::new(root)?;
    match request {
        NativeFsRunnerRequest::ListFiles(args) => list_files(&context, args),
        NativeFsRunnerRequest::Glob(args) => glob(&context, args),
        NativeFsRunnerRequest::Grep(args) => grep(&context, args),
    }
}

fn list_files(context: &RunnerContext, args: ListFilesArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let entries = collect_entries(context, &dir, args.recursive, args.max_entries.max(1))?;
    let metadata = ListFilesMetadata {
        ok: true,
        status: "success",
        tool: "list_files",
        path: context.display_path(&dir),
        recursive: args.recursive,
        returned_count: entries.items.len(),
        total_count: total_count(entries.items.len(), entries.truncated),
        truncated: entries.truncated,
        default_ignored: default_ignored_names(),
        summary: summarize_entries(&entries.items),
    };
    let output = ListFilesOutput {
        metadata,
        entries: entries.items,
    };
    render_tool_output(
        &output.metadata,
        format_entries("entries", &output.entries),
        &output,
        args.raw_json,
    )
}

fn glob(context: &RunnerContext, args: GlobArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let pattern = Pattern::new(&args.pattern)
        .with_context(|| format!("invalid glob pattern {}", args.pattern))?;
    let matches = collect_glob_matches(context, &dir, &pattern, args.max_matches.max(1))?;
    let output = GlobOutput {
        metadata: GlobMetadata {
            ok: true,
            status: "success",
            tool: "glob",
            pattern: args.pattern,
            path: context.display_path(&dir),
            returned_count: matches.items.len(),
            total_count: total_count(matches.items.len(), matches.truncated),
            truncated: matches.truncated,
            default_ignored: default_ignored_names(),
        },
        matches: matches.items,
    };
    render_tool_output(
        &output.metadata,
        format_entries("matches", &output.matches),
        &output,
        args.raw_json,
    )
}

fn grep(context: &RunnerContext, args: GrepArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let collected = collect_grep_matches(
        context,
        &dir,
        &args.pattern,
        args.case_sensitive,
        args.max_matches.max(1),
    )?;
    let mut files_with_matches = BTreeSet::new();
    let matches = collected
        .items
        .into_iter()
        .map(|entry| {
            files_with_matches.insert(entry.path.clone());
            GrepOutputMatch {
                path: entry.path,
                line_number: entry.line_number,
                preview: truncate_grep_preview(&entry.line),
            }
        })
        .collect::<Vec<_>>();

    let output = GrepOutput {
        metadata: GrepMetadata {
            ok: true,
            status: "success",
            tool: "grep",
            pattern: args.pattern,
            path: context.display_path(&dir),
            case_sensitive: args.case_sensitive,
            returned_count: matches.len(),
            total_count: total_count(matches.len(), collected.truncated),
            files_with_matches: files_with_matches.len(),
            truncated: collected.truncated,
            default_ignored: default_ignored_names(),
        },
        matches,
    };
    render_tool_output(
        &output.metadata,
        format_grep_matches(&output.matches),
        &output,
        args.raw_json,
    )
}
