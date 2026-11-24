use std::path::Path;

use anyhow::Result;
use globset::GlobMatcher;

use crate::context::{HeldDir, RunnerContext};
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{admit_next, descend, sorted_children, Admitted, Sweep, WalkState};

pub(crate) fn collect_glob_matches(
    context: &RunnerContext,
    dir: &HeldDir,
    pattern: &GlobMatcher,
    max_matches: usize,
    walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut sweep = Sweep::new(walk);
    collect_glob_matches_inner(context, &dir.path, dir, pattern, max_matches, &mut sweep)?;
    Ok(sweep.finish())
}

fn collect_glob_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &HeldDir,
    pattern: &GlobMatcher,
    max_matches: usize,
    sweep: &mut Sweep<FilesystemEntry>,
) -> Result<()> {
    let pushed = sweep.ignores.push_dir(context, &dir.path);
    for entry in sorted_children(context, dir, &mut sweep.walk)? {
        if sweep.truncated || sweep.walk.exhausted() {
            break;
        }
        let (path, kind) = match admit_next(
            context,
            traversal_root,
            dir,
            &entry,
            &mut sweep.walk,
            &sweep.ignores,
        ) {
            Admitted::Skip => continue,
            Admitted::Stop => break,
            Admitted::Entry { path, kind } => (path, kind),
        };
        let is_dir = Admitted::is_dir(kind);
        let display = context.display_path(&path);
        if pattern.is_match(&display) {
            if sweep.items.len() >= max_matches {
                sweep.truncated = true;
                break;
            }
            sweep.items.push(FilesystemEntry {
                path: display,
                entry_type: if is_dir { "directory" } else { "file" },
            });
        }
        if is_dir {
            if let Some(child) = descend(context, dir, &path) {
                collect_glob_matches_inner(
                    context,
                    traversal_root,
                    &child,
                    pattern,
                    max_matches,
                    sweep,
                )?;
            }
        }
    }
    sweep.ignores.pop(pushed);
    Ok(())
}
