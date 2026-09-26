use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use globset::GlobBuilder;

use crate::context::RunnerContext;
use crate::output::{
    default_ignored_names, echoed_pattern, format_entries, format_grep_matches, render_tool_output,
    render_within_budget, summarize_entries, total_count, truncate_grep_preview, GlobMetadata,
    GlobOutput, GrepMetadata, GrepOutput, GrepOutputMatch, ListFilesMetadata, ListFilesOutput,
    MAX_RESPONSE_BYTES,
};
use crate::protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};
use crate::traversal::{
    collect_entries, collect_glob_matches, collect_grep_matches, CollectedGrep, WalkLimits,
    WalkState,
};

const DEFAULT_MAX_ENTRIES_VISITED: usize = 200_000;
const SEARCH_DIR_ENTRY_HINTS: usize = 10;
const DEFAULT_MAX_BYTES_READ: u64 = 128 * 1024 * 1024;
const DEFAULT_MAX_WALL_MS: u64 = 15_000;
/// Longest glob or grep pattern accepted. Pattern compilers have their own
/// size limits (globset panics past its regex limit), so an oversized pattern
/// is refused up front with an error instead of crashing the runner.
const MAX_PATTERN_BYTES: usize = 4096;

fn check_pattern_size(pattern: &str) -> Result<()> {
    anyhow::ensure!(
        pattern.len() <= MAX_PATTERN_BYTES,
        "pattern is {} bytes; at most {MAX_PATTERN_BYTES} are accepted",
        pattern.len()
    );
    Ok(())
}

fn walk_state(
    max_entries_visited: Option<usize>,
    max_bytes_read: Option<u64>,
    max_wall_ms: Option<u64>,
) -> WalkState {
    WalkState::new(WalkLimits {
        max_entries_visited: max_entries_visited
            .unwrap_or(DEFAULT_MAX_ENTRIES_VISITED)
            .max(1),
        max_bytes_read: max_bytes_read.unwrap_or(DEFAULT_MAX_BYTES_READ).max(1),
        max_wall: Duration::from_millis(max_wall_ms.unwrap_or(DEFAULT_MAX_WALL_MS).max(1)),
    })
}

fn top_level_entry_names(dir: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::with_capacity(SEARCH_DIR_ENTRY_HINTS + 1);
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if names.len() == SEARCH_DIR_ENTRY_HINTS && names.last().is_some_and(|last| *last <= name) {
            continue;
        }
        let position = names
            .binary_search(&name)
            .unwrap_or_else(|position| position);
        names.insert(position, name);
        names.truncate(SEARCH_DIR_ENTRY_HINTS);
    }
    names
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
    let path = context.display_path(&dir);
    render_within_budget(
        &entries.items,
        entries.truncated || entries.walk.budget_exhausted,
        MAX_RESPONSE_BYTES,
        |items, truncated| {
            let output = ListFilesOutput {
                metadata: ListFilesMetadata {
                    ok: true,
                    status: "success",
                    tool: "list_files",
                    path: path.clone(),
                    recursive: args.recursive,
                    returned_count: items.len(),
                    total_count: total_count(items.len(), truncated),
                    truncated,
                    default_ignored: default_ignored_names(),
                    summary: summarize_entries(items),
                    walk: entries.walk.clone(),
                },
                entries: items.to_vec(),
            };
            render_tool_output(
                &output.metadata,
                format_entries("entries", &output.entries),
                &output,
                args.raw_json,
            )
        },
    )
}

fn glob_literal_prefix(pattern: &str) -> Vec<&str> {
    let components: Vec<&str> = pattern.split('/').collect();
    let mut literal = Vec::new();
    for (index, component) in components.iter().enumerate() {
        if index == components.len() - 1
            || component.is_empty()
            || *component == "."
            || *component == ".."
            || component.contains(['*', '?', '[', ']', '{', '}'])
        {
            break;
        }
        literal.push(*component);
    }
    literal
}

fn glob(context: &RunnerContext, args: GlobArgs) -> Result<String> {
    check_pattern_size(&args.pattern)?;
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let pattern = GlobBuilder::new(&args.pattern)
        .build()
        .with_context(|| format!("invalid glob pattern {}", args.pattern))?
        .compile_matcher();
    let walk = walk_state(args.max_entries_visited, None, args.max_wall_ms);
    let prefix = glob_literal_prefix(&args.pattern);
    let pattern_prefix = (!prefix.is_empty()).then(|| prefix.join("/"));
    let (walk_dir, pattern_prefix_exists) =
        if prefix.is_empty() || !dir.starts_with(context.base_dir()) {
            (Some(dir.clone()), true)
        } else {
            match context.resolve_prune_subdir(&prefix) {
                Some(prefix_dir) => {
                    if prefix_dir.starts_with(&dir) {
                        (Some(prefix_dir), true)
                    } else if dir.starts_with(&prefix_dir) {
                        (Some(dir.clone()), true)
                    } else {
                        (None, true)
                    }
                }
                None => (None, false),
            }
        };
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
    let search_dir_entries = (matches.items.is_empty() && !matches.walk.budget_exhausted)
        .then(|| top_level_entry_names(&dir));
    let path = context.display_path(&dir);
    render_within_budget(
        &matches.items,
        matches.truncated || matches.walk.budget_exhausted,
        MAX_RESPONSE_BYTES,
        |items, truncated| {
            let output = GlobOutput {
                metadata: GlobMetadata {
                    ok: true,
                    status: "success",
                    tool: "glob",
                    pattern: echoed_pattern(&args.pattern),
                    pattern_prefix: pattern_prefix.as_deref().map(echoed_pattern),
                    pattern_prefix_exists,
                    search_dir_entries: search_dir_entries.clone(),
                    path: path.clone(),
                    returned_count: items.len(),
                    total_count: total_count(items.len(), truncated),
                    truncated,
                    default_ignored: default_ignored_names(),
                    walk: matches.walk.clone(),
                },
                matches: items.to_vec(),
            };
            render_tool_output(
                &output.metadata,
                format_entries("matches", &output.matches),
                &output,
                args.raw_json,
            )
        },
    )
}

fn grep(context: &RunnerContext, args: GrepArgs) -> Result<String> {
    check_pattern_size(&args.pattern)?;
    let path = context.resolve_existing_path(args.path.as_deref())?;
    let walk = walk_state(
        args.max_entries_visited,
        args.max_bytes_read,
        args.max_wall_ms,
    );
    let CollectedGrep {
        collected,
        bytes_read,
        file_stats,
        pattern_syntax,
    } = collect_grep_matches(
        context,
        &path,
        &args.pattern,
        args.case_sensitive,
        args.max_matches.max(1),
        walk,
    )?;
    let matches = collected
        .items
        .into_iter()
        .map(|entry| GrepOutputMatch {
            path: entry.path,
            line_number: entry.line_number,
            preview: truncate_grep_preview(&entry.line),
        })
        .collect::<Vec<_>>();
    let search_dir_entries =
        (matches.is_empty() && path.is_dir() && !collected.walk.budget_exhausted)
            .then(|| top_level_entry_names(&path));
    let display_path = context.display_path(&path);
    render_within_budget(
        &matches,
        collected.truncated || collected.walk.budget_exhausted,
        MAX_RESPONSE_BYTES,
        |items, truncated| {
            let files_with_matches = items
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<BTreeSet<_>>()
                .len();
            let output = GrepOutput {
                metadata: GrepMetadata {
                    ok: true,
                    status: "success",
                    tool: "grep",
                    pattern: echoed_pattern(&args.pattern),
                    pattern_syntax,
                    search_dir_entries: search_dir_entries.clone(),
                    path: display_path.clone(),
                    case_sensitive: args.case_sensitive,
                    returned_count: items.len(),
                    total_count: total_count(items.len(), truncated),
                    files_with_matches,
                    truncated,
                    default_ignored: default_ignored_names(),
                    bytes_read,
                    skipped_large_files: file_stats.skipped_large_files,
                    skipped_binary_files: file_stats.skipped_binary_files,
                    walk: collected.walk.clone(),
                },
                matches: items.to_vec(),
            };
            render_tool_output(
                &output.metadata,
                format_grep_matches(&output.matches),
                &output,
                args.raw_json,
            )
        },
    )
}
