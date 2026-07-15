use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use glob::Pattern;

use crate::context::RunnerContext;
use crate::output::{
    default_ignored_names, format_entries, format_grep_matches, render_tool_output,
    summarize_entries, total_count, truncate_grep_preview, GlobMetadata, GlobOutput, GrepMetadata,
    GrepOutput, GrepOutputMatch, ListFilesMetadata, ListFilesOutput,
};
use crate::protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};
use crate::traversal::{
    collect_entries, collect_glob_matches, collect_grep_matches, WalkLimits, WalkState,
};

// Walk budgets (#729): bound what a single search may do before returning
// partial results with explicit exhaustion metadata. Without these, a
// zero-match pattern over a large tool root (a home directory is ~6M entries)
// walks everything and looks like a hang to the model.
const DEFAULT_MAX_ENTRIES_VISITED: usize = 200_000;
const DEFAULT_MAX_BYTES_READ: u64 = 128 * 1024 * 1024;
const DEFAULT_MAX_WALL_MS: u64 = 15_000;

fn walk_state(
    max_entries_visited: Option<usize>,
    max_bytes_read: Option<u64>,
    max_wall_ms: Option<u64>,
) -> WalkState {
    WalkState::new(WalkLimits {
        max_entries_visited: max_entries_visited.unwrap_or(DEFAULT_MAX_ENTRIES_VISITED).max(1),
        max_bytes_read: max_bytes_read.unwrap_or(DEFAULT_MAX_BYTES_READ).max(1),
        max_wall: Duration::from_millis(max_wall_ms.unwrap_or(DEFAULT_MAX_WALL_MS).max(1)),
    })
}

pub fn execute_request(root: PathBuf, request: NativeFsRunnerRequest) -> Result<String> {
    execute_request_with_base(root, None, request)
}

pub fn execute_request_with_base(
    root: PathBuf,
    base: Option<PathBuf>,
    request: NativeFsRunnerRequest,
) -> Result<String> {
    let context = RunnerContext::new_with_base(root, base)?;
    match request {
        NativeFsRunnerRequest::ListFiles(args) => list_files(&context, args),
        NativeFsRunnerRequest::Glob(args) => glob(&context, args),
        NativeFsRunnerRequest::Grep(args) => grep(&context, args),
    }
}

fn list_files(context: &RunnerContext, args: ListFilesArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let walk = walk_state(args.max_entries_visited, None, args.max_wall_ms);
    let entries = collect_entries(context, &dir, args.recursive, args.max_entries.max(1), walk)?;
    let truncated = entries.truncated || entries.walk.budget_exhausted;
    let metadata = ListFilesMetadata {
        ok: true,
        status: "success",
        tool: "list_files",
        path: context.display_path(&dir),
        recursive: args.recursive,
        returned_count: entries.items.len(),
        total_count: total_count(entries.items.len(), truncated),
        truncated,
        default_ignored: default_ignored_names(),
        summary: summarize_entries(&entries.items),
        walk: entries.walk,
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

/// Leading literal path components of a glob pattern. The walk only needs to
/// enter this subtree — nothing outside it can ever match (#729). The final
/// component is the filename position and is matched, not walked; `.`/`..`
/// and metacharacter components end the prefix.
fn glob_literal_prefix(pattern: &str) -> Vec<&str> {
    let components: Vec<&str> = pattern.split('/').collect();
    let mut literal = Vec::new();
    for (index, component) in components.iter().enumerate() {
        if index == components.len() - 1
            || component.is_empty()
            || *component == "."
            || *component == ".."
            || component.contains(['*', '?', '[', ']'])
        {
            break;
        }
        literal.push(*component);
    }
    literal
}

fn glob(context: &RunnerContext, args: GlobArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let pattern = Pattern::new(&args.pattern)
        .with_context(|| format!("invalid glob pattern {}", args.pattern))?;
    let walk = walk_state(args.max_entries_visited, None, args.max_wall_ms);
    let prefix = glob_literal_prefix(&args.pattern);
    let pattern_prefix = (!prefix.is_empty()).then(|| prefix.join("/"));
    let walk_dir = if prefix.is_empty() {
        Some(dir.clone())
    } else {
        context.resolve_prune_subdir(&dir, &prefix)
    };
    let pattern_prefix_exists = walk_dir.is_some();
    let matches = match walk_dir {
        Some(walk_dir) => {
            collect_glob_matches(context, &walk_dir, &pattern, args.max_matches.max(1), walk)?
        }
        None => crate::model::Collected {
            items: Vec::new(),
            truncated: false,
            walk: walk.into_stats(),
        },
    };
    let truncated = matches.truncated || matches.walk.budget_exhausted;
    let output = GlobOutput {
        metadata: GlobMetadata {
            ok: true,
            status: "success",
            tool: "glob",
            pattern: args.pattern,
            pattern_prefix,
            pattern_prefix_exists,
            path: context.display_path(&dir),
            returned_count: matches.items.len(),
            total_count: total_count(matches.items.len(), truncated),
            truncated,
            default_ignored: default_ignored_names(),
            walk: matches.walk,
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
    let path = context.resolve_existing_path(args.path.as_deref())?;
    let walk = walk_state(args.max_entries_visited, args.max_bytes_read, args.max_wall_ms);
    let collected = collect_grep_matches(
        context,
        &path,
        &args.pattern,
        args.case_sensitive,
        args.max_matches.max(1),
        walk,
    )?;
    let bytes_read = collected.bytes_read;
    let collected = collected.collected;
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

    let truncated = collected.truncated || collected.walk.budget_exhausted;
    let output = GrepOutput {
        metadata: GrepMetadata {
            ok: true,
            status: "success",
            tool: "grep",
            pattern: args.pattern,
            path: context.display_path(&path),
            case_sensitive: args.case_sensitive,
            returned_count: matches.len(),
            total_count: total_count(matches.len(), truncated),
            files_with_matches: files_with_matches.len(),
            truncated,
            default_ignored: default_ignored_names(),
            bytes_read,
            walk: collected.walk,
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
