use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::context::RunnerContext;
use crate::model::{Collected, WalkStats, DEFAULT_IGNORED_NAMES};

pub(crate) struct WalkLimits {
    pub(crate) max_entries_visited: usize,
    pub(crate) max_bytes_read: u64,
    pub(crate) max_wall: Duration,
}

pub(crate) struct Sweep<T> {
    pub(crate) items: Vec<T>,
    pub(crate) truncated: bool,
    pub(crate) walk: WalkState,
    pub(crate) ignores: GitignoreStack,
}

impl<T> Sweep<T> {
    pub(crate) fn new(walk: WalkState) -> Self {
        Self {
            items: Vec::new(),
            truncated: false,
            walk,
            ignores: GitignoreStack::new(),
        }
    }

    pub(crate) fn finish(self) -> Collected<T> {
        Collected {
            items: self.items,
            truncated: self.truncated,
            walk: self.walk.into_stats(),
        }
    }
}

pub(crate) struct WalkState {
    limits: WalkLimits,
    started: Instant,
    entries_visited: usize,
    bytes_read: u64,
    stopped_at: Option<String>,
    exhausted: bool,
    default_ignored: std::collections::BTreeSet<&'static str>,
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
            default_ignored: std::collections::BTreeSet::new(),
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
            default_ignored: self.default_ignored.into_iter().collect(),
        }
    }

    /// Whether `path` carries a default-ignored name below `traversal_root`;
    /// the names it skipped are reported with the walk.
    pub(crate) fn skips_default_ignored(&mut self, traversal_root: &Path, path: &Path) -> bool {
        if path == traversal_root {
            return false;
        }
        let Some(name) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| {
                DEFAULT_IGNORED_NAMES
                    .iter()
                    .find(|ignored| **ignored == name)
            })
        else {
            return false;
        };
        self.default_ignored.insert(name);
        true
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
    #[cfg(feature = "test-fixtures")]
    if let Some(blocker) = context.blocker() {
        blocker.block(dir);
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
    if walk.skips_default_ignored(traversal_root, &path) || ignores.is_ignored(&path, is_dir) {
        return Admitted::Skip;
    }
    if !walk.admit_entry(context, &path) {
        return Admitted::Stop;
    }
    Admitted::Entry { path, is_dir }
}

fn should_skip_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}
