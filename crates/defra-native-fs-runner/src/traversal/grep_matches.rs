use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, GrepMatch};
use crate::traversal::common::{
    should_ignore_path, should_skip_io_error, sorted_children, WalkState,
};

pub(crate) fn collect_grep_matches(
    context: &RunnerContext,
    path: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
    mut walk: WalkState,
) -> Result<CollectedGrep> {
    let mut items = Vec::new();
    let mut truncated = false;
    let needle = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };
    let traversal_root = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    if path.is_dir() {
        collect_grep_matches_inner(
            context,
            traversal_root,
            path,
            &needle,
            case_sensitive,
            max_matches,
            &mut items,
            &mut truncated,
            &mut walk,
        )?;
    } else {
        walk.admit_entry(context, path);
        grep_file(
            context,
            traversal_root,
            path,
            &needle,
            case_sensitive,
            max_matches,
            &mut items,
            &mut truncated,
            &mut walk,
        )?;
    }
    let bytes_read = walk.bytes_read();
    Ok(CollectedGrep {
        collected: Collected {
            items,
            truncated,
            walk: walk.into_stats(),
        },
        bytes_read,
    })
}

pub(crate) struct CollectedGrep {
    pub(crate) collected: Collected<GrepMatch>,
    pub(crate) bytes_read: u64,
}

#[allow(clippy::too_many_arguments)]
fn collect_grep_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    needle: &str,
    case_sensitive: bool,
    max_matches: usize,
    matches: &mut Vec<GrepMatch>,
    truncated: &mut bool,
    walk: &mut WalkState,
) -> Result<()> {
    for entry in sorted_children(dir)? {
        if *truncated || walk.exhausted() {
            break;
        }
        let path = entry.path();
        if should_ignore_path(traversal_root, &path) {
            continue;
        }
        if !walk.admit_entry(context, &path) {
            break;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if should_skip_io_error(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        if metadata.is_dir() {
            collect_grep_matches_inner(
                context,
                traversal_root,
                &path,
                needle,
                case_sensitive,
                max_matches,
                matches,
                truncated,
                walk,
            )?;
            continue;
        }
        grep_file(
            context,
            traversal_root,
            &path,
            needle,
            case_sensitive,
            max_matches,
            matches,
            truncated,
            walk,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn grep_file(
    context: &RunnerContext,
    traversal_root: &Path,
    path: &Path,
    needle: &str,
    case_sensitive: bool,
    max_matches: usize,
    matches: &mut Vec<GrepMatch>,
    truncated: &mut bool,
    walk: &mut WalkState,
) -> Result<()> {
    if should_ignore_path(traversal_root, path) {
        return Ok(());
    }
    let Ok(file_metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if !walk.admit_bytes(context, path, file_metadata.len()) {
        return Ok(());
    }
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(_) => return Ok(()),
    };
    for (idx, line) in contents.lines().enumerate() {
        if matches.len() >= max_matches {
            *truncated = true;
            break;
        }
        let haystack = if case_sensitive {
            line.to_string()
        } else {
            line.to_lowercase()
        };
        if haystack.contains(needle) {
            matches.push(GrepMatch {
                path: context.display_path(path),
                line_number: idx + 1,
                line: line.to_string(),
            });
        }
    }
    Ok(())
}
