use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use anyhow::Result;

use crate::beneath::{Access, ChildKind, Intent, Located};
use crate::context::{HeldDir, RunnerContext};
use crate::model::{Collected, GrepMatch};
use crate::traversal::common::{admit_next, descend, sorted_children, Admitted, Sweep, WalkState};

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
    target: &Located,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
    walk: WalkState,
) -> Result<CollectedGrep> {
    let mut sweep = Sweep::new(walk);
    let mut file_stats = GrepFileStats::default();
    let needle = Needle::new(pattern, case_sensitive);
    let path = target.path();
    let traversal_root = if target.is_dir() {
        path.clone()
    } else {
        path.parent().unwrap_or(&path).to_path_buf()
    };
    let scan = GrepScan {
        context,
        traversal_root: &traversal_root,
        needle: &needle,
        max_matches,
    };
    if target.is_dir() {
        let dir = HeldDir {
            fd: target.open_dir()?,
            path: path.clone(),
        };
        collect_grep_matches_inner(&scan, &dir, &mut sweep, &mut file_stats)?;
    } else {
        sweep.walk.admit_entry(context, &path);
        let file = target.open(Access::Read).ok();
        grep_file(&scan, &path, file, &mut sweep, &mut file_stats, true)?;
    }
    let bytes_read = sweep.walk.bytes_read();
    Ok(CollectedGrep {
        collected: sweep.finish(),
        bytes_read,
        file_stats,
        pattern_syntax: needle.syntax,
    })
}

struct GrepScan<'a> {
    context: &'a RunnerContext,
    traversal_root: &'a Path,
    needle: &'a Needle,
    max_matches: usize,
}

fn collect_grep_matches_inner(
    scan: &GrepScan<'_>,
    dir: &HeldDir,
    sweep: &mut Sweep<GrepMatch>,
    file_stats: &mut GrepFileStats,
) -> Result<()> {
    let pushed = sweep.ignores.push_dir(scan.context, &dir.path);
    for entry in sorted_children(scan.context, dir, &mut sweep.walk)? {
        if sweep.truncated || sweep.walk.exhausted() {
            break;
        }
        let (path, kind) = match admit_next(
            scan.context,
            scan.traversal_root,
            dir,
            &entry,
            &mut sweep.walk,
            &sweep.ignores,
        ) {
            Admitted::Skip => continue,
            Admitted::Stop => break,
            Admitted::Entry { path, kind } => (path, kind),
        };
        let file = match kind {
            ChildKind::Directory => {
                if let Some(child) = descend(scan.context, dir, &path) {
                    collect_grep_matches_inner(scan, &child, sweep, file_stats)?;
                }
                continue;
            }
            ChildKind::File => scan
                .context
                .root()
                .child_file(&dir.fd, &dir.path, &entry.name),
            ChildKind::Link => linked_file(scan.context, &path),
            ChildKind::Other => continue,
        };
        grep_file(scan, &path, file, sweep, file_stats, false)?;
    }
    sweep.ignores.pop(pushed);
    Ok(())
}

/// The regular file a link in the walk leads to, resolved again beneath the
/// root; `None` when it leads out of the root or to no regular file.
fn linked_file(context: &RunnerContext, path: &Path) -> Option<File> {
    let below = context.below_root(path)?;
    context
        .root()
        .locate_relative(below, Intent::Existing)
        .and_then(|located| located.open(Access::Read))
        .ok()
}

fn grep_file(
    scan: &GrepScan<'_>,
    path: &Path,
    file: Option<File>,
    sweep: &mut Sweep<GrepMatch>,
    file_stats: &mut GrepFileStats,
    explicit_file: bool,
) -> Result<()> {
    if sweep.walk.skips_default_ignored(scan.traversal_root, path) {
        return Ok(());
    }
    let Some(mut file) = file else {
        return Ok(());
    };
    let Ok(file_metadata) = file.metadata() else {
        return Ok(());
    };
    if !explicit_file && file_metadata.len() > MAX_GREP_FILE_BYTES {
        file_stats.skipped_large_files += 1;
        return Ok(());
    }
    if !sweep
        .walk
        .admit_bytes(scan.context, path, file_metadata.len())
    {
        return Ok(());
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Ok(());
    }
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        file_stats.skipped_binary_files += 1;
        return Ok(());
    }
    let contents = String::from_utf8_lossy(&bytes);
    for (idx, line) in contents.lines().enumerate() {
        if sweep.items.len() >= scan.max_matches {
            sweep.truncated = true;
            break;
        }
        if scan.needle.matches_line(line) {
            sweep.items.push(GrepMatch {
                path: scan.context.display_path(path),
                line_number: idx + 1,
                line: line.to_string(),
            });
        }
    }
    Ok(())
}
