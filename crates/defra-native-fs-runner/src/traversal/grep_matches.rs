use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, GrepMatch};
use crate::traversal::common::{
    should_ignore_path, should_skip_io_error, sorted_children, WalkState,
};

/// Per-file bound: files larger than this are skipped (and counted) rather
/// than read whole into memory. Production greps over a home-directory tool
/// root were reading multi-GB logs and model files end to end (#729).
const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// A NUL byte in this leading window classifies the file as binary.
const BINARY_SNIFF_BYTES: usize = 4096;

/// Search needle prepared once per grep. The ASCII case-insensitive form
/// avoids the per-line `to_lowercase` allocation the old implementation paid
/// on every line of every file scanned.
enum Needle {
    Exact(String),
    AsciiCi(String),
    UnicodeCi(String),
}

impl Needle {
    fn new(pattern: &str, case_sensitive: bool) -> Self {
        if case_sensitive {
            Needle::Exact(pattern.to_string())
        } else if pattern.is_ascii() {
            Needle::AsciiCi(pattern.to_ascii_lowercase())
        } else {
            Needle::UnicodeCi(pattern.to_lowercase())
        }
    }

    fn matches_line(&self, line: &str) -> bool {
        match self {
            Needle::Exact(needle) => line.contains(needle.as_str()),
            Needle::AsciiCi(needle) => contains_ascii_ci(line.as_bytes(), needle.as_bytes()),
            Needle::UnicodeCi(needle) => line.to_lowercase().contains(needle.as_str()),
        }
    }
}

/// Allocation-free ASCII case-insensitive substring search. Multi-byte UTF-8
/// units are all >= 0x80 and never ASCII-case-equal to the (ASCII) needle
/// bytes, so scanning raw bytes is sound.
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[derive(Default)]
pub(crate) struct GrepFileStats {
    pub(crate) skipped_large_files: usize,
    pub(crate) skipped_binary_files: usize,
}

pub(crate) struct CollectedGrep {
    pub(crate) collected: Collected<GrepMatch>,
    pub(crate) bytes_read: u64,
    pub(crate) file_stats: GrepFileStats,
}

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
    let mut file_stats = GrepFileStats::default();
    let needle = Needle::new(pattern, case_sensitive);
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
            max_matches,
            &mut items,
            &mut truncated,
            &mut walk,
            &mut file_stats,
        )?;
    } else {
        walk.admit_entry(context, path);
        grep_file(
            context,
            traversal_root,
            path,
            &needle,
            max_matches,
            &mut items,
            &mut truncated,
            &mut walk,
            &mut file_stats,
            true,
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
        file_stats,
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_grep_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    needle: &Needle,
    max_matches: usize,
    matches: &mut Vec<GrepMatch>,
    truncated: &mut bool,
    walk: &mut WalkState,
    file_stats: &mut GrepFileStats,
) -> Result<()> {
    for entry in sorted_children(context, dir, walk)? {
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
                max_matches,
                matches,
                truncated,
                walk,
                file_stats,
            )?;
            continue;
        }
        grep_file(
            context,
            traversal_root,
            &path,
            needle,
            max_matches,
            matches,
            truncated,
            walk,
            file_stats,
            false,
        )?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn grep_file(
    context: &RunnerContext,
    traversal_root: &Path,
    path: &Path,
    needle: &Needle,
    max_matches: usize,
    matches: &mut Vec<GrepMatch>,
    truncated: &mut bool,
    walk: &mut WalkState,
    file_stats: &mut GrepFileStats,
    explicit_file: bool,
) -> Result<()> {
    if should_ignore_path(traversal_root, path) {
        return Ok(());
    }
    let Ok(file_metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    // The per-file size cap is a tree-walk guard. A file the caller named
    // explicitly must be searched (the byte budget still bounds it, and an
    // over-budget read reports exhaustion instead of silence).
    if !explicit_file && file_metadata.len() > MAX_GREP_FILE_BYTES {
        file_stats.skipped_large_files += 1;
        return Ok(());
    }
    if !walk.admit_bytes(context, path, file_metadata.len()) {
        return Ok(());
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Ok(());
    };
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        file_stats.skipped_binary_files += 1;
        return Ok(());
    }
    let contents = String::from_utf8_lossy(&bytes);
    for (idx, line) in contents.lines().enumerate() {
        if matches.len() >= max_matches {
            *truncated = true;
            break;
        }
        if needle.matches_line(line) {
            matches.push(GrepMatch {
                path: context.display_path(path),
                line_number: idx + 1,
                line: line.to_string(),
            });
        }
    }
    Ok(())
}
