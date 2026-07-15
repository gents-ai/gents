use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{WalkStats, DEFAULT_IGNORED_NAMES};

pub(crate) struct WalkLimits {
    pub(crate) max_entries_visited: usize,
    pub(crate) max_bytes_read: u64,
    pub(crate) max_wall: Duration,
}

/// Mutable walk accounting threaded through every traversal. Budgets stop the
/// walk with partial results instead of letting a zero-match search scan an
/// unbounded tree (#729).
pub(crate) struct WalkState {
    limits: WalkLimits,
    started: Instant,
    entries_visited: usize,
    bytes_read: u64,
    stopped_at: Option<String>,
    exhausted: bool,
}

impl WalkState {
    pub(crate) fn new(limits: WalkLimits) -> Self {
        Self {
            limits,
            started: Instant::now(),
            entries_visited: 0,
            bytes_read: 0,
            stopped_at: None,
            exhausted: false,
        }
    }

    /// Admit one directory entry into the walk. Returns false when a budget
    /// is exhausted; the caller must stop without processing the entry.
    pub(crate) fn admit_entry(&mut self, context: &RunnerContext, path: &Path) -> bool {
        if self.exhausted {
            return false;
        }
        if self.entries_visited >= self.limits.max_entries_visited
            || self.started.elapsed() >= self.limits.max_wall
        {
            self.stop_at(context, path);
            return false;
        }
        self.entries_visited += 1;
        true
    }

    /// Admit reading `bytes` of file content. Returns false when the byte
    /// budget would be exceeded; the caller must skip the read and stop.
    pub(crate) fn admit_bytes(&mut self, context: &RunnerContext, path: &Path, bytes: u64) -> bool {
        if self.exhausted {
            return false;
        }
        if self.bytes_read.saturating_add(bytes) > self.limits.max_bytes_read {
            self.stop_at(context, path);
            return false;
        }
        self.bytes_read += bytes;
        true
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(crate) fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub(crate) fn into_stats(self) -> WalkStats {
        WalkStats {
            entries_visited: self.entries_visited,
            elapsed_ms: self.started.elapsed().as_millis() as u64,
            budget_exhausted: self.exhausted,
            stopped_at: self.stopped_at,
        }
    }

    fn stop_at(&mut self, context: &RunnerContext, path: &Path) {
        self.exhausted = true;
        self.stopped_at = Some(context.display_path(path));
    }
}

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
