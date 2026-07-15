use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{
    should_ignore_path, sorted_children, GitignoreStack, WalkState,
};

pub(crate) fn collect_entries(
    context: &RunnerContext,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    mut walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut items = Vec::new();
    let mut truncated = false;
    let mut ignores = GitignoreStack::new();
    collect_entries_inner(
        context,
        dir,
        dir,
        recursive,
        max_entries,
        &mut items,
        &mut truncated,
        &mut walk,
        &mut ignores,
    )?;
    Ok(Collected {
        items,
        truncated,
        walk: walk.into_stats(),
    })
}

#[allow(clippy::too_many_arguments)]
fn collect_entries_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    entries: &mut Vec<FilesystemEntry>,
    truncated: &mut bool,
    walk: &mut WalkState,
    ignores: &mut GitignoreStack,
) -> Result<()> {
    let pushed = ignores.push_dir(dir);
    for entry in sorted_children(context, dir, walk)? {
        if *truncated || walk.exhausted() {
            break;
        }
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let is_dir = file_type.is_dir();
        if should_ignore_path(traversal_root, &path) || ignores.is_ignored(&path, is_dir) {
            continue;
        }
        if !walk.admit_entry(context, &path) {
            break;
        }
        if entries.len() >= max_entries {
            *truncated = true;
            break;
        }
        entries.push(FilesystemEntry {
            path: context.display_path(&path),
            entry_type: if is_dir { "directory" } else { "file" },
        });
        if recursive && is_dir {
            collect_entries_inner(
                context,
                traversal_root,
                &path,
                true,
                max_entries,
                entries,
                truncated,
                walk,
                ignores,
            )?;
        }
    }
    ignores.pop(pushed);
    Ok(())
}
