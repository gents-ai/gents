use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use glob::Pattern;
use tokio::process::Command;

#[derive(Clone)]
pub(super) struct ToolContext {
    root: Arc<PathBuf>,
}

#[derive(Debug)]
pub(super) struct ToolError(pub(super) anyhow::Error);

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ToolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.root_cause())
    }
}

impl From<anyhow::Error> for ToolError {
    fn from(error: anyhow::Error) -> Self {
        Self(error)
    }
}

impl From<std::io::Error> for ToolError {
    fn from(error: std::io::Error) -> Self {
        Self(error.into())
    }
}

impl ToolContext {
    pub(super) fn from_home() -> Result<Self> {
        let root = dirs::home_dir()
            .or_else(|| std::env::current_dir().ok())
            .ok_or_else(|| anyhow!("unable to determine a tool root directory"))?;
        Self::new(root, false)
    }

    pub(super) fn new(root: PathBuf, create_missing: bool) -> Result<Self> {
        if create_missing && !root.exists() {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("creating tool root {}", root.display()))?;
        }

        let canonical = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing tool root {}", root.display()))?;

        Ok(Self {
            root: Arc::new(canonical),
        })
    }

    pub(super) fn root(&self) -> &Path {
        self.root.as_path()
    }

    pub(super) fn resolve_path_allow_create(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            normalize_for_creation(candidate)?
        } else {
            normalize_for_creation(&self.root.join(candidate))?
        };
        self.ensure_allowed(resolved)
    }

    pub(super) fn resolve_path(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let resolved = if candidate.is_absolute() {
            std::fs::canonicalize(candidate)
                .with_context(|| format!("resolving path {}", candidate.display()))?
        } else {
            let joined = self.root.join(candidate);
            std::fs::canonicalize(&joined)
                .with_context(|| format!("resolving path {}", joined.display()))?
        };
        self.ensure_allowed(resolved)
    }

    pub(super) fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => (*self.root).clone(),
        };

        if !resolved.is_dir() {
            bail!("path is not a directory: {}", resolved.display());
        }

        Ok(resolved)
    }

    pub(super) fn resolve_existing_file(&self, path: &str) -> Result<PathBuf> {
        let resolved = self.resolve_path(path)?;
        if !resolved.is_file() {
            bail!("path is not a file: {}", resolved.display());
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf> {
        if path.starts_with(self.root.as_path()) {
            Ok(path)
        } else {
            bail!(
                "path is outside the allowed tool root {}: {}",
                self.root.display(),
                path.display()
            );
        }
    }

    pub(super) fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(self.root.as_path())
            .ok()
            .map(|relative| {
                let display = relative.to_string_lossy().replace('\\', "/");
                if display.is_empty() {
                    ".".to_string()
                } else {
                    display
                }
            })
            .unwrap_or_else(|| path.display().to_string())
    }
}

fn normalize_for_creation(path: &Path) -> Result<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}

pub(super) async fn run_command(
    context: &ToolContext,
    command_name: &str,
    args: &[String],
    cwd: Option<&str>,
    timeout: Duration,
) -> Result<String> {
    let cwd = context.resolve_existing_dir(cwd)?;
    let mut command = Command::new(command_name);
    command
        .args(args)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("NO_COLOR", "1")
        .env("CLICOLOR", "0")
        .env("TERM", "dumb");

    let output = tokio::time::timeout(timeout, command.output())
        .await
        .with_context(|| format!("timed out after {}s", timeout.as_secs()))??;

    let stdout = truncate_text(
        &String::from_utf8_lossy(&output.stdout),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let stderr = truncate_text(
        &String::from_utf8_lossy(&output.stderr),
        super::DEFAULT_MAX_COMMAND_CHARS,
    );
    let exit_code = output.status.code().unwrap_or(-1);
    let command_line = std::iter::once(command_name)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");

    Ok(format!(
        "cwd: {}\ncommand: {}\nexit_code: {}\nstdout:\n{}\nstderr:\n{}",
        context.display_path(&cwd),
        command_line,
        exit_code,
        if stdout.is_empty() { "(empty)" } else { &stdout },
        if stderr.is_empty() { "(empty)" } else { &stderr },
    ))
}

pub(super) fn collect_entries(
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

pub(super) fn collect_glob_matches(
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

pub(super) fn collect_grep_matches(
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

pub(super) fn render_file_contents(
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

pub(super) fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }

    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}\n[truncated to {max_chars} chars]")
}

pub(super) fn validate_read_only_command(
    command: &str,
    args: &[String],
    allowlist: &[String],
) -> Result<()> {
    if !allowlist.iter().any(|allowed| allowed == command) {
        bail!("command is not allowed by the read-only bash tool: {command}");
    }

    match command {
        "sed" => {
            if args
                .iter()
                .any(|arg| arg == "-i" || arg == "--in-place" || arg.starts_with("-i"))
            {
                bail!("sed in-place edits are not allowed");
            }
        }
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    arg.as_str(),
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprint"
                        | "-fprintf"
                        | "-fls"
                )
            }) {
                bail!("find arguments that can write or execute are not allowed");
            }
        }
        "git" => validate_git_args(args)?,
        _ => {}
    }

    Ok(())
}

fn validate_git_args(args: &[String]) -> Result<()> {
    let subcommand = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| anyhow!("git requires a read-only subcommand"))?;

    match subcommand {
        "status" | "diff" | "show" | "log" | "ls-files" | "grep" | "branch" | "rev-parse" => {
            Ok(())
        }
        other => bail!("git subcommand is not allowed by the read-only bash tool: {other}"),
    }
}

pub(super) fn default_max_list_entries() -> usize {
    super::DEFAULT_MAX_LIST_ENTRIES
}

pub(super) fn default_max_file_chars() -> usize {
    super::DEFAULT_MAX_FILE_CHARS
}

pub(super) fn default_max_matches() -> usize {
    super::DEFAULT_MAX_MATCHES
}

pub(super) fn default_command_timeout_secs() -> u64 {
    super::DEFAULT_COMMAND_TIMEOUT_SECS
}
