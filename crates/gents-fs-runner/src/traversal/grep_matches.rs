use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, GrepMatch};
use crate::traversal::common::{
    admit_next, should_ignore_path, sorted_children, Admitted, GitignoreStack, WalkState,
};

const MAX_GREP_FILE_BYTES: u64 = 2 * 1024 * 1024;
const BINARY_SNIFF_BYTES: usize = 4096;

struct Needle {
    regex: regex::Regex,
    syntax: &'static str,
}

const REGEX_SIZE_LIMIT: usize = 10 * (1 << 20);

impl Needle {
    fn new(pattern: &str, case_sensitive: bool) -> Self {
        let compile = |source: &str| {
            regex::RegexBuilder::new(source)
                .case_insensitive(!case_sensitive)
                .size_limit(REGEX_SIZE_LIMIT)
                .build()
        };
        match compile(pattern) {
            Ok(regex) => Needle {
                regex,
                syntax: "regex",
            },
            Err(_) => Needle {
                regex: compile(&regex::escape(pattern)).expect("escaped literal always compiles"),
                syntax: "literal",
            },
        }
    }

    fn matches_line(&self, line: &str) -> bool {
        self.regex.is_match(line)
    }
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
    pub(crate) pattern_syntax: &'static str,
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
    let path_is_dir = path.is_dir();
    let traversal_root = if path_is_dir {
        path
    } else {
        path.parent().unwrap_or(path)
    };
    if path_is_dir {
        let mut ignores = GitignoreStack::new();
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
            &mut ignores,
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
        pattern_syntax: needle.syntax,
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
    ignores: &mut GitignoreStack,
) -> Result<()> {
    let pushed = ignores.push_dir(dir);
    for entry in sorted_children(context, dir, walk)? {
        if *truncated || walk.exhausted() {
            break;
        }
        let (path, is_dir) = match admit_next(context, traversal_root, &entry, walk, ignores) {
            Admitted::Skip => continue,
            Admitted::Stop => break,
            Admitted::Entry { path, is_dir } => (path, is_dir),
        };
        if is_dir {
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
                ignores,
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
    ignores.pop(pushed);
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
