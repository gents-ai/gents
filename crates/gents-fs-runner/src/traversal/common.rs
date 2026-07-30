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

    pub(crate) fn dir_scan_should_stop(
        &mut self,
        context: &RunnerContext,
        dir: &Path,
        collected: usize,
    ) -> bool {
        if self.exhausted {
            return true;
        }
        if self.started.elapsed() >= self.limits.max_wall {
            self.stop_at(context, dir);
            return true;
        }
        let remaining = self
            .limits
            .max_entries_visited
            .saturating_sub(self.entries_visited);
        collected > remaining
    }

    fn stop_at(&mut self, context: &RunnerContext, path: &Path) {
        self.exhausted = true;
        self.stopped_at = Some(context.display_path(path));
    }
}

pub(super) fn sorted_children(
    context: &RunnerContext,
    dir: &Path,
    walk: &mut WalkState,
) -> Result<Vec<std::fs::DirEntry>> {
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
        if walk.dir_scan_should_stop(context, dir, children.len()) {
            break;
        }
        match entry {
            Ok(entry) => children.push(entry),
            Err(error) if should_skip_io_error(&error) => continue,
            Err(error) => return Err(error.into()),
        }
    }
    children.sort_by_key(|entry| entry.file_name());
    Ok(children)
}

/// their generated junk filtered before it consumes walk budget. Note:
pub(crate) struct GitignoreStack {
    stack: Vec<ignore::gitignore::Gitignore>,
}

impl GitignoreStack {
    pub(crate) fn new() -> Self {
        Self { stack: Vec::new() }
    }

    pub(crate) fn push_dir(&mut self, dir: &Path) -> usize {
        let file = dir.join(".gitignore");
        if !file.is_file() {
            return 0;
        }
        let mut builder = ignore::gitignore::GitignoreBuilder::new(dir);
        builder.add(&file);
        match builder.build() {
            Ok(matcher) => {
                self.stack.push(matcher);
                1
            }
            Err(_) => 0,
        }
    }

    pub(crate) fn pop(&mut self, pushed: usize) {
        for _ in 0..pushed {
            self.stack.pop();
        }
    }

    pub(crate) fn is_ignored(&self, path: &Path, is_dir: bool) -> bool {
        for matcher in self.stack.iter().rev() {
            match matcher.matched(path, is_dir) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
                ignore::Match::None => {}
            }
        }
        false
    }
}

pub(super) enum Admitted {
    Skip,
    Stop,
    Entry {
        path: std::path::PathBuf,
        is_dir: bool,
    },
}

pub(super) fn admit_next(
    context: &RunnerContext,
    traversal_root: &Path,
    entry: &std::fs::DirEntry,
    walk: &mut WalkState,
    ignores: &GitignoreStack,
) -> Admitted {
    let path = entry.path();
    let Ok(file_type) = entry.file_type() else {
        return Admitted::Skip;
    };
    let is_dir = file_type.is_dir();
    if should_ignore_path(traversal_root, &path) || ignores.is_ignored(&path, is_dir) {
        return Admitted::Skip;
    }
    if !walk.admit_entry(context, &path) {
        return Admitted::Stop;
    }
    Admitted::Entry { path, is_dir }
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
    let target = std::env::var("GENTS_FS_RUNNER_BLOCK_DIR").ok()?;
    if Path::new(&target) != dir {
        return None;
    }
    let millis = std::env::var("GENTS_FS_RUNNER_BLOCK_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    Some(std::time::Duration::from_millis(millis))
}

fn should_skip_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context_and_dir(entries: usize) -> (RunnerContext, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "gents-fs-common-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..entries {
            std::fs::write(dir.join(format!("f-{index:03}")), "x").unwrap();
        }
        let context = RunnerContext::new_with_base(dir.clone(), None).unwrap();
        (context, std::fs::canonicalize(&dir).unwrap())
    }

    fn limits(max_entries: usize, max_wall: Duration) -> WalkLimits {
        WalkLimits {
            max_entries_visited: max_entries,
            max_bytes_read: u64::MAX,
            max_wall,
        }
    }

    #[test]
    fn dir_scan_stops_collecting_at_entry_budget() {
        let (context, dir) = test_context_and_dir(100);
        let mut walk = WalkState::new(limits(5, Duration::from_secs(60)));
        let children = sorted_children(&context, &dir, &mut walk).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            children.len() <= 6,
            "collected {} dirents past the entry budget",
            children.len()
        );
    }

    #[test]
    fn dir_scan_stops_on_expired_wall_budget() {
        let (context, dir) = test_context_and_dir(100);
        let mut walk = WalkState::new(limits(1000, Duration::ZERO));
        let children = sorted_children(&context, &dir, &mut walk).unwrap();
        let stats_exhausted = walk.exhausted();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(children.is_empty(), "collected {}", children.len());
        assert!(stats_exhausted);
    }
}
