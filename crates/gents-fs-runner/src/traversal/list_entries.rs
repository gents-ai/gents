use std::path::Path;

use anyhow::Result;

use crate::context::{HeldDir, RunnerContext};
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{admit_next, descend, sorted_children, Admitted, Sweep, WalkState};

pub(crate) fn collect_entries(
    context: &RunnerContext,
    dir: &HeldDir,
    recursive: bool,
    max_entries: usize,
    walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut sweep = Sweep::new(walk);
    collect_entries_inner(context, &dir.path, dir, recursive, max_entries, &mut sweep)?;
    Ok(sweep.finish())
}

fn collect_entries_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &HeldDir,
    recursive: bool,
    max_entries: usize,
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
        if sweep.items.len() >= max_entries {
            sweep.truncated = true;
            break;
        }
        let is_dir = Admitted::is_dir(kind);
        sweep.items.push(FilesystemEntry {
            path: context.display_path(&path),
            entry_type: if is_dir { "directory" } else { "file" },
        });
        if recursive && is_dir {
            if let Some(child) = descend(context, dir, &path) {
                collect_entries_inner(context, traversal_root, &child, true, max_entries, sweep)?;
            }
        }
    }
    sweep.ignores.pop(pushed);
    Ok(())
}
