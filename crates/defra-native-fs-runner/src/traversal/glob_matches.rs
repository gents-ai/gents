use std::path::Path;

use anyhow::Result;
use globset::GlobMatcher;

use crate::context::RunnerContext;
use crate::model::{Collected, FilesystemEntry};
use crate::traversal::common::{
    should_ignore_path, sorted_children, GitignoreStack, WalkState,
};

pub(crate) fn collect_glob_matches(
    context: &RunnerContext,
    dir: &Path,
    pattern: &GlobMatcher,
    max_matches: usize,
    mut walk: WalkState,
) -> Result<Collected<FilesystemEntry>> {
    let mut items = Vec::new();
    let mut truncated = false;
    let mut ignores = GitignoreStack::new();
    collect_glob_matches_inner(
        context,
        dir,
        dir,
        pattern,
        max_matches,
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
fn collect_glob_matches_inner(
    context: &RunnerContext,
    traversal_root: &Path,
    dir: &Path,
    pattern: &GlobMatcher,
    max_matches: usize,
    matches: &mut Vec<FilesystemEntry>,
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
        // file_type comes straight from the dirent — no stat per entry.
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
        let display = context.display_path(&path);
        if pattern.is_match(&display) {
            if matches.len() >= max_matches {
                *truncated = true;
                break;
            }
            matches.push(FilesystemEntry {
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
                matches,
                truncated,
                walk,
                ignores,
            )?;
        }
    }
    ignores.pop(pushed);
    Ok(())
}
