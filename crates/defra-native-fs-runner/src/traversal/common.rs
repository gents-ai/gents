use std::path::Path;

use anyhow::Result;

use crate::model::DEFAULT_IGNORED_NAMES;

pub(super) fn sorted_children(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    if let Some(duration) = sorted_children_block_for_test(dir) {
        std::thread::sleep(duration);
    }
    let read_dir = match std::fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(error) if should_skip_io_error(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut children = Vec::new();
    for entry in read_dir {
        match entry {
            Ok(entry) => children.push(entry),
            Err(error) if should_skip_io_error(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    children.sort_by_key(|entry| entry.file_name());
    Ok(children)
}

pub(super) fn should_ignore_path(traversal_root: &Path, path: &Path) -> bool {
    if path == traversal_root {
        return false;
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| DEFAULT_IGNORED_NAMES.contains(&name))
        .unwrap_or(false)
}

fn sorted_children_block_for_test(dir: &Path) -> Option<std::time::Duration> {
    let target = std::env::var("DEFRA_NATIVE_FS_RUNNER_BLOCK_DIR").ok()?;
    if Path::new(&target) != dir {
        return None;
    }
    let millis = std::env::var("DEFRA_NATIVE_FS_RUNNER_BLOCK_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Some(std::time::Duration::from_millis(millis))
}

pub(super) fn should_skip_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}
