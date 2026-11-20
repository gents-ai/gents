use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{admit_next, sorted_children, Admitted, Sweep, WalkState};

pub(crate) fn collect_entries(
    context: &RunnerContext,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut sweep = Sweep::new(walk);
    collect_entries_inner(context, dir, dir, recursive, max_entries, &mut sweep)?;
    Ok(sweep.finish())
}

fn collect_entries_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
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
        if sweep.items.len() >= max_entries {
            sweep.truncated = true;
            break;
        }
        sweep.items.push(FilesystemEntry {
            path: context.display_path(&path),
            entry_type: if is_dir { "directory" } else { "file" },
        });
        if recursive && is_dir {
            collect_entries_inner(context, traversal_root, &path, true, max_entries, sweep)?;
        }
    }
    sweep.ignores.pop(pushed);
    Ok(())
}
