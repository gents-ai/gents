use std::path::Path;

use anyhow::Result;
use glob::Pattern;
use serde::Serialize;

use super::context::ToolContext;

const DEFAULT_IGNORED_NAMES: &[&str] = &[
    ".cache",
    ".direnv",
    ".git",
    ".next",
    ".turbo",
    ".venv",
    "dist",
    "node_modules",
    "target",
    "venv",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FilesystemEntry {
    pub path: String,
    pub entry_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GrepMatch {
    pub path: String,
    pub line_number: usize,
    pub line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Collected<T> {
    pub items: Vec<T>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RenderedFileContents {
    pub content: String,
    pub total_lines: usize,
    pub returned_lines: usize,
    pub start_line: usize,
    pub end_line: usize,
}

pub(crate) fn collect_entries(
    context: &ToolContext,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
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
    )?;
    Ok(Collected { items, truncated })
}

pub(crate) fn collect_glob_matches(
    context: &ToolContext,
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

pub(crate) fn collect_grep_matches(
    context: &ToolContext,
    dir: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
) -> Result<Collected<GrepMatch>> {
    let mut items = Vec::new();
    let mut truncated = false;
    collect_grep_matches_inner(
        context,
        dir,
        dir,
        pattern,
        case_sensitive,
        max_matches,
        &mut items,
        &mut truncated,
    )?;
    Ok(Collected { items, truncated })
}

pub(crate) fn render_file_contents(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> RenderedFileContents {
    let total_lines = text.lines().count();
    let start_line = start_line.unwrap_or(1).max(1);
    let end_line = end_line
        .unwrap_or(total_lines.max(start_line))
        .max(start_line);

    let mut rendered = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_number = idx + 1;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        rendered.push(format!("L{line_number}: {line}"));
    }

    let content = if rendered.is_empty() && text.is_empty() {
        "(empty file)".to_string()
    } else if rendered.is_empty() {
        "(no lines in requested range)".to_string()
    } else {
        rendered.join("\n")
    };

    RenderedFileContents {
        content,
        total_lines,
        returned_lines: rendered.len(),
        start_line,
        end_line,
    }
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[truncated to {max_chars} chars]")
}

pub(crate) fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}... [truncated]")
}

pub(crate) fn default_ignored_names() -> &'static [&'static str] {
    DEFAULT_IGNORED_NAMES
}

fn collect_entries_inner(
    context: &ToolContext,
    traversal_root: &Path,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    entries: &mut Vec<FilesystemEntry>,
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
            )?;
        }
    }

    Ok(())
}

fn collect_glob_matches_inner(
    context: &ToolContext,
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

#[allow(clippy::too_many_arguments)]
fn collect_grep_matches_inner(
    context: &ToolContext,
    traversal_root: &Path,
    dir: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
    matches: &mut Vec<GrepMatch>,
    truncated: &mut bool,
) -> Result<()> {
    let needle = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };

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

        if metadata.is_dir() {
            collect_grep_matches_inner(
                context,
                traversal_root,
                &path,
                pattern,
                case_sensitive,
                max_matches,
                matches,
                truncated,
            )?;
            continue;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };

        for (idx, line) in contents.lines().enumerate() {
            if matches.len() >= max_matches {
                *truncated = true;
                break;
            }

            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if haystack.contains(&needle) {
                matches.push(GrepMatch {
                    path: context.display_path(&path),
                    line_number: idx + 1,
                    line: line.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn sorted_children(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    #[cfg(test)]
    maybe_block_sorted_children_for_test(dir);

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

fn should_ignore_path(traversal_root: &Path, path: &Path) -> bool {
    if path == traversal_root {
        return false;
    }

    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| DEFAULT_IGNORED_NAMES.contains(&name))
        .unwrap_or(false)
}

fn should_skip_io_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}

#[cfg(test)]
mod blocking_test_hook {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    static BLOCKED_DIRS: OnceLock<Mutex<Vec<(PathBuf, Duration)>>> = OnceLock::new();

    fn blocked_dirs() -> &'static Mutex<Vec<(PathBuf, Duration)>> {
        BLOCKED_DIRS.get_or_init(|| Mutex::new(Vec::new()))
    }

    pub(crate) struct SortedChildrenBlockGuard {
        dir: PathBuf,
    }

    impl Drop for SortedChildrenBlockGuard {
        fn drop(&mut self) {
            let mut blocked = blocked_dirs()
                .lock()
                .expect("sorted-children test hook mutex poisoned");
            blocked.retain(|(dir, _)| dir != &self.dir);
        }
    }

    pub(crate) fn block_next_sorted_children_for_test(
        dir: impl AsRef<Path>,
        duration: Duration,
    ) -> SortedChildrenBlockGuard {
        let dir = dir.as_ref().to_path_buf();
        blocked_dirs()
            .lock()
            .expect("sorted-children test hook mutex poisoned")
            .push((dir.clone(), duration));
        SortedChildrenBlockGuard { dir }
    }

    pub(super) fn maybe_block_sorted_children_for_test(dir: &Path) {
        let duration = {
            let mut blocked = blocked_dirs()
                .lock()
                .expect("sorted-children test hook mutex poisoned");
            blocked
                .iter()
                .position(|(blocked_dir, _)| blocked_dir == dir)
                .map(|index| blocked.remove(index).1)
        };

        if let Some(duration) = duration {
            std::thread::sleep(duration);
        }
    }
}

#[cfg(test)]
pub(crate) use blocking_test_hook::block_next_sorted_children_for_test;

#[cfg(test)]
use blocking_test_hook::maybe_block_sorted_children_for_test;
