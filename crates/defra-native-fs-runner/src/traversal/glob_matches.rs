use std::path::Path;

use anyhow::Result;
use glob::Pattern;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{should_ignore_path, should_skip_io_error, sorted_children};

pub(crate) fn collect_glob_matches(
    context: &RunnerContext,
    dir: &Path,
    pattern: &Pattern,
    max_matches: usize,
) -> Result<Collected<FilesystemEntry>> {
    let mut items = Vec::new();
    let mut truncated = false;
    collect_glob_matches_inner(
        context,
        dir,
        dir,
        pattern,
        max_matches,
        &mut items,
        &mut truncated,
    )?;
    Ok(Collected { items, truncated })
}

fn collect_glob_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    pattern: &Pattern,
    max_matches: usize,
    matches: &mut Vec<FilesystemEntry>,
    truncated: &mut bool,
) -> Result<()> {
    for entry in sorted_children(dir)? {
        if *truncated {
            break;
        }
        let path = entry.path();
        if should_ignore_path(traversal_root, &path) {
            continue;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) if should_skip_io_error(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        let display = context.display_path(&path);
        if pattern.matches(&display) {
            if matches.len() >= max_matches {
                *truncated = true;
                break;
            }
            matches.push(FilesystemEntry {
                path: display,
                entry_type: if metadata.is_dir() {
                    "directory"
                } else {
                    "file"
                },
            });
        }
        if metadata.is_dir() {
            collect_glob_matches_inner(
                context,
                traversal_root,
                &path,
                pattern,
                max_matches,
                matches,
                truncated,
            )?;
        }
    }
    Ok(())
}
