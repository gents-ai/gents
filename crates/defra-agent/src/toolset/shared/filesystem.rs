use std::path::Path;

use anyhow::Result;
use glob::Pattern;

use super::context::ToolContext;

pub(crate) fn collect_entries(
    context: &ToolContext,
    dir: &Path,
    recursive: bool,
    max_entries: usize,
    entries: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if entries.len() >= max_entries {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        let mut display = context.display_path(&path);
        if metadata.is_dir() {
            display.push('/');
        }
        entries.push(display);

        if recursive && metadata.is_dir() && entries.len() < max_entries {
            collect_entries(context, &path, true, max_entries, entries)?;
        }
    }

    Ok(())
}

pub(crate) fn collect_glob_matches(
    context: &ToolContext,
    dir: &Path,
    pattern: &Pattern,
    max_matches: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if matches.len() >= max_matches {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        let display = context.display_path(&path);
        if pattern.matches(&display) {
            matches.push(if metadata.is_dir() {
                format!("{display}/")
            } else {
                display
            });
        }

        if metadata.is_dir() && matches.len() < max_matches {
            collect_glob_matches(context, &path, pattern, max_matches, matches)?;
        }
    }

    Ok(())
}

pub(crate) fn collect_grep_matches(
    context: &ToolContext,
    dir: &Path,
    pattern: &str,
    case_sensitive: bool,
    max_matches: usize,
    matches: &mut Vec<String>,
) -> Result<()> {
    let mut children = std::fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        if matches.len() >= max_matches {
            break;
        }

        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            collect_grep_matches(context, &path, pattern, case_sensitive, max_matches, matches)?;
            continue;
        }

        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            Err(_) => continue,
        };

        let needle = if case_sensitive {
            pattern.to_string()
        } else {
            pattern.to_lowercase()
        };

        for (idx, line) in contents.lines().enumerate() {
            if matches.len() >= max_matches {
                break;
            }
            let haystack = if case_sensitive {
                line.to_string()
            } else {
                line.to_lowercase()
            };
            if haystack.contains(&needle) {
                matches.push(format!(
                    "{}:{}:{}",
                    context.display_path(&path),
                    idx + 1,
                    line
                ));
            }
        }
    }

    Ok(())
}

pub(crate) fn render_file_contents(
    text: &str,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> String {
    let start_line = start_line.unwrap_or(1).max(1);
    let end_line = end_line.unwrap_or(usize::MAX).max(start_line);

    let mut rendered = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let line_number = idx + 1;
        if line_number < start_line || line_number > end_line {
            continue;
        }
        rendered.push(format!("{line_number}: {line}"));
    }

    if rendered.is_empty() && text.is_empty() {
        "(empty file)".to_string()
    } else if rendered.is_empty() {
        "(no lines in requested range)".to_string()
    } else {
        rendered.join("\n")
    }
}

pub(crate) fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[truncated to {max_chars} chars]")
}
