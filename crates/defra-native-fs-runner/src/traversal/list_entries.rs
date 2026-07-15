use std::path::Path;

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{
    should_ignore_path, should_skip_io_error, sorted_children, WalkState,
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
    collect_entries_inner(
        context,
        dir,
        dir,
        recursive,
        max_entries,
        &mut items,
        &mut truncated,
        &mut walk,
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
        if entries.len() >= max_entries {
            *truncated = true;
            break;
        }
        entries.push(FilesystemEntry {
            path: context.display_path(&path),
            entry_type: if metadata.is_dir() {
                "directory"
            } else {
                "file"
            },
        });
        if recursive && metadata.is_dir() {
            collect_entries_inner(
                context,
                traversal_root,
                &path,
                true,
                max_entries,
                entries,
                truncated,
                walk,
            )?;
        }
    }
    Ok(())
}
