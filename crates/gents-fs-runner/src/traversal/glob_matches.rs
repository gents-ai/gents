use std::path::Path;

use anyhow::Result;
use globset::GlobMatcher;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{admit_next, sorted_children, Admitted, Sweep, WalkState};

pub(crate) fn collect_glob_matches(
    context: &RunnerContext,
    dir: &Path,
    pattern: &GlobMatcher,
    max_matches: usize,
    walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut sweep = Sweep::new(walk);
    collect_glob_matches_inner(context, dir, dir, pattern, max_matches, &mut sweep)?;
    Ok(sweep.finish())
}

fn collect_glob_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    pattern: &GlobMatcher,
    max_matches: usize,
    sweep: &mut Sweep<FilesystemEntry>,
) -> Result<()> {
    let pushed = sweep.ignores.push_dir(dir);
    for entry in sorted_children(context, dir, &mut sweep.walk)? {
        if sweep.truncated || sweep.walk.exhausted() {
            break;
        }
        let (path, is_dir) = match admit_next(
            context,
            traversal_root,
            &entry,
            &mut sweep.walk,
            &sweep.ignores,
        ) {
            Admitted::Skip => continue,
            Admitted::Stop => break,
            Admitted::Entry { path, is_dir } => (path, is_dir),
        };
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
            collect_glob_matches_inner(
                context,
                traversal_root,
                &path,
                pattern,
                max_matches,
                sweep,
            )?;
        }
    }
    sweep.ignores.pop(pushed);
    Ok(())
}
