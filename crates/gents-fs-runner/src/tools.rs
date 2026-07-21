use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use globset::GlobBuilder;

use crate::context::RunnerContext;
use crate::output::{
    default_ignored_names, format_entries, format_grep_matches, render_tool_output,
    summarize_entries, total_count, truncate_grep_preview, GlobMetadata, GlobOutput, GrepMetadata,
    GrepOutput, GrepOutputMatch, ListFilesMetadata, ListFilesOutput,
};
use crate::protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};
use crate::traversal::{
    collect_entries, collect_glob_matches, collect_grep_matches, CollectedGrep, WalkLimits,
    WalkState,
};

// Walk budgets (#729): bound what a single search may do before returning
// partial results with explicit exhaustion metadata. Without these, a
// zero-match pattern over a large tool root (a home directory is ~6M entries)
// walks everything and looks like a hang to the model.
const DEFAULT_MAX_ENTRIES_VISITED: usize = 200_000;
/// How many top-level names a zero-match diagnostic reveals (#729): enough to
/// correct a wrong path anchor, small enough to stay cheap.
const SEARCH_DIR_ENTRY_HINTS: usize = 10;
const DEFAULT_MAX_BYTES_READ: u64 = 128 * 1024 * 1024;
const DEFAULT_MAX_WALL_MS: u64 = 15_000;

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

/// Top-level entry names of the searched directory, attached to zero-match
/// results so a wrong anchor is visible on the first attempt.
fn top_level_entry_names(dir: &Path) -> Vec<String> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    // Keep only the lexicographically smallest names while scanning: the
    // searched directory can be huge, and allocating + sorting every entry
    // name would re-do unbudgeted work the walk just gave up on.
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
            || component.contains(['*', '?', '[', ']', '{', '}'])
        {
            break;
        }
        literal.push(*component);
    }
    literal
}

fn glob(context: &RunnerContext, args: GlobArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let pattern = GlobBuilder::new(&args.pattern)
        .build()
        .with_context(|| format!("invalid glob pattern {}", args.pattern))?
        .compile_matcher();
    let walk = walk_state(args.max_entries_visited, None, args.max_wall_ms);
    let prefix = glob_literal_prefix(&args.pattern);
    let pattern_prefix = (!prefix.is_empty()).then(|| prefix.join("/"));
    // The pattern is matched against BASE-relative display paths, so its
    // literal prefix resolves against the base — never joined onto the path
    // argument, which would apply the prefix twice when the path already
    // lies inside it. The walk is the intersection of the path subtree and
    // the prefix subtree; when the two are disjoint nothing can match and no
    // walk happens at all. An empty prefix prunes nothing, and an absolute
    // path argument outside the base is walked as-is (display paths are not
    // base-relative there, so pruning assumptions do not hold).
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
    let truncated = matches.truncated || matches.walk.budget_exhausted;
    // Anchor hint only when the zero-match is a genuine miss: a budget-
    // stopped search says nothing about the anchor being wrong.
    let search_dir_entries = (matches.items.is_empty() && !matches.walk.budget_exhausted)
        .then(|| top_level_entry_names(&dir));
    let output = GlobOutput {
        metadata: GlobMetadata {
            ok: true,
            status: "success",
            tool: "glob",
            pattern: args.pattern,
            pattern_prefix,
            pattern_prefix_exists,
            search_dir_entries,
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
    // Anchor hint only when the zero-match is a genuine directory-search
    // miss: a single-file grep has no anchor to correct, and a budget-
    // stopped search says nothing about the anchor being wrong.
    let search_dir_entries =
        (matches.is_empty() && path.is_dir() && !collected.walk.budget_exhausted)
            .then(|| top_level_entry_names(&path));
    let output = GrepOutput {
        metadata: GrepMetadata {
            ok: true,
            status: "success",
            tool: "grep",
            pattern: args.pattern,
            pattern_syntax,
            search_dir_entries,
            path: context.display_path(&path),
            case_sensitive: args.case_sensitive,
            returned_count: matches.len(),
            total_count: total_count(matches.len(), truncated),
            files_with_matches: files_with_matches.len(),
            truncated,
            default_ignored: default_ignored_names(),
            bytes_read,
            skipped_large_files: file_stats.skipped_large_files,
            skipped_binary_files: file_stats.skipped_binary_files,
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
