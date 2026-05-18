use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use glob::Pattern;
use serde::Serialize;

pub mod protocol;

use protocol::{GlobArgs, GrepArgs, ListFilesArgs, NativeFsRunnerRequest};

const DEFAULT_GREP_PREVIEW_CHARS: usize = 240;
const OUTPUT_META_PREFIX: &str = "defra_fs: ";
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
struct FilesystemEntry {
    path: String,
    entry_type: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GrepMatch {
    path: String,
    line_number: usize,
    line: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Collected<T> {
    items: Vec<T>,
    truncated: bool,
}

#[derive(Clone)]
struct RunnerContext {
    root: PathBuf,
}

impl RunnerContext {
    fn new(root: PathBuf) -> Result<Self> {
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("canonicalizing runner root {}", root.display()))?;
        Ok(Self { root })
    }

    fn resolve_path(&self, path: &str) -> Result<PathBuf> {
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

    fn resolve_existing_dir(&self, path: Option<&str>) -> Result<PathBuf> {
        let resolved = match path {
            Some(path) if !path.trim().is_empty() => self.resolve_path(path)?,
            _ => self.root.clone(),
        };
        if !resolved.is_dir() {
            bail!("path is not a directory: {}", resolved.display());
        }
        Ok(resolved)
    }

    fn ensure_allowed(&self, path: PathBuf) -> Result<PathBuf> {
        if path.starts_with(&self.root) {
            Ok(path)
        } else {
            bail!(
                "path is outside the allowed tool root {}: {}",
                self.root.display(),
                path.display()
            );
        }
    }

    fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
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

pub fn execute_request(root: PathBuf, request: NativeFsRunnerRequest) -> Result<String> {
    let context = RunnerContext::new(root)?;
    match request {
        NativeFsRunnerRequest::ListFiles(args) => list_files(&context, args),
        NativeFsRunnerRequest::Glob(args) => glob(&context, args),
        NativeFsRunnerRequest::Grep(args) => grep(&context, args),
    }
}

fn list_files(context: &RunnerContext, args: ListFilesArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let entries = collect_entries(context, &dir, args.recursive, args.max_entries.max(1))?;
    let metadata = ListFilesMetadata {
        ok: true,
        status: "success",
        tool: "list_files",
        path: context.display_path(&dir),
        recursive: args.recursive,
        returned_count: entries.items.len(),
        total_count: total_count(entries.items.len(), entries.truncated),
        truncated: entries.truncated,
        default_ignored: DEFAULT_IGNORED_NAMES,
        summary: summarize_entries(&entries.items),
    };
    let output = ListFilesOutput {
        metadata,
        entries: entries.items,
    };
    render_tool_output(
        &output.metadata,
        format_entries("entries", &output.entries),
        &output,
        args.raw_json,
    )
}

fn glob(context: &RunnerContext, args: GlobArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let pattern = Pattern::new(&args.pattern)
        .with_context(|| format!("invalid glob pattern {}", args.pattern))?;
    let matches = collect_glob_matches(context, &dir, &pattern, args.max_matches.max(1))?;
    let output = GlobOutput {
        metadata: GlobMetadata {
            ok: true,
            status: "success",
            tool: "glob",
            pattern: args.pattern,
            path: context.display_path(&dir),
            returned_count: matches.items.len(),
            total_count: total_count(matches.items.len(), matches.truncated),
            truncated: matches.truncated,
            default_ignored: DEFAULT_IGNORED_NAMES,
        },
        matches: matches.items,
    };
    render_tool_output(
        &output.metadata,
        format_entries("matches", &output.matches),
        &output,
        args.raw_json,
    )
}

fn grep(context: &RunnerContext, args: GrepArgs) -> Result<String> {
    let dir = context.resolve_existing_dir(args.path.as_deref())?;
    let collected = collect_grep_matches(
        context,
        &dir,
        &args.pattern,
        args.case_sensitive,
        args.max_matches.max(1),
    )?;
    let mut files_with_matches = BTreeSet::new();
    let matches = collected
        .items
        .into_iter()
        .map(|entry| {
            files_with_matches.insert(entry.path.clone());
            GrepOutputMatch {
                path: entry.path,
                line_number: entry.line_number,
                preview: truncate_inline(&entry.line, DEFAULT_GREP_PREVIEW_CHARS),
            }
        })
        .collect::<Vec<_>>();

    let output = GrepOutput {
        metadata: GrepMetadata {
            ok: true,
            status: "success",
            tool: "grep",
            pattern: args.pattern,
            path: context.display_path(&dir),
            case_sensitive: args.case_sensitive,
            returned_count: matches.len(),
            total_count: total_count(matches.len(), collected.truncated),
            files_with_matches: files_with_matches.len(),
            truncated: collected.truncated,
            default_ignored: DEFAULT_IGNORED_NAMES,
        },
        matches,
    };
    render_tool_output(
        &output.metadata,
        format_grep_matches(&output.matches),
        &output,
        args.raw_json,
    )
}

fn collect_entries(
    context: &RunnerContext,
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

fn collect_glob_matches(
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

fn collect_grep_matches(
    context: &RunnerContext,
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

fn collect_entries_inner(
    context: &RunnerContext,
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

#[allow(clippy::too_many_arguments)]
fn collect_grep_matches_inner(
    context: &RunnerContext,
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

#[derive(Serialize)]
struct EntrySummary {
    files: usize,
    directories: usize,
}

#[derive(Serialize)]
struct ListFilesMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    path: String,
    recursive: bool,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    default_ignored: &'static [&'static str],
    summary: EntrySummary,
}

#[derive(Serialize)]
struct ListFilesOutput {
    #[serde(flatten)]
    metadata: ListFilesMetadata,
    entries: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct GlobMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    pattern: String,
    path: String,
    returned_count: usize,
    total_count: Option<usize>,
    truncated: bool,
    default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
struct GlobOutput {
    #[serde(flatten)]
    metadata: GlobMetadata,
    matches: Vec<FilesystemEntry>,
}

#[derive(Serialize)]
struct GrepMetadata {
    ok: bool,
    status: &'static str,
    tool: &'static str,
    pattern: String,
    path: String,
    case_sensitive: bool,
    returned_count: usize,
    total_count: Option<usize>,
    files_with_matches: usize,
    truncated: bool,
    default_ignored: &'static [&'static str],
}

#[derive(Serialize)]
struct GrepOutput {
    #[serde(flatten)]
    metadata: GrepMetadata,
    matches: Vec<GrepOutputMatch>,
}

#[derive(Serialize)]
struct GrepOutputMatch {
    path: String,
    line_number: usize,
    preview: String,
}

fn summarize_entries(entries: &[FilesystemEntry]) -> EntrySummary {
    let mut files = 0;
    let mut directories = 0;
    for entry in entries {
        match entry.entry_type {
            "file" => files += 1,
            "directory" => directories += 1,
            _ => {}
        }
    }
    EntrySummary { files, directories }
}

fn total_count(returned_count: usize, truncated: bool) -> Option<usize> {
    (!truncated).then_some(returned_count)
}

fn format_entries(label: &str, entries: &[FilesystemEntry]) -> String {
    let mut out = String::from(label);
    out.push(':');
    if entries.is_empty() {
        out.push_str("\n(none)");
        return out;
    }
    for entry in entries {
        out.push('\n');
        out.push_str(entry.entry_type);
        out.push(' ');
        out.push_str(&entry.path);
    }
    out
}

fn format_grep_matches(matches: &[GrepOutputMatch]) -> String {
    let mut out = String::from("matches:");
    if matches.is_empty() {
        out.push_str("\n(none)");
        return out;
    }
    for entry in matches {
        out.push('\n');
        out.push_str(&entry.path);
        out.push_str(":L");
        out.push_str(&entry.line_number.to_string());
        out.push_str(": ");
        out.push_str(&entry.preview);
    }
    out
}

fn render_tool_output(
    metadata: &impl Serialize,
    body: String,
    raw_value: &impl Serialize,
    raw_json: bool,
) -> Result<String> {
    if raw_json {
        return render_json(raw_value);
    }
    let mut out = String::from(OUTPUT_META_PREFIX);
    out.push_str(&render_json(metadata)?);
    if !body.is_empty() {
        out.push('\n');
        out.push_str(&body);
    }
    Ok(out)
}

fn render_json(value: &impl Serialize) -> Result<String> {
    serde_json::to_string(value).context("serializing tool output")
}

fn truncate_inline(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let truncated = text.chars().take(max_chars).collect::<String>();
    format!("{truncated}... [truncated]")
}

pub fn self_test() -> Result<()> {
    let root = std::env::temp_dir().join(format!(
        "defra-native-fs-runner-self-test-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let output = execute_request(
        root.clone(),
        NativeFsRunnerRequest::Glob(GlobArgs {
            pattern: "**/*.rs".to_string(),
            path: Some(".".to_string()),
            max_matches: 10,
            raw_json: false,
        }),
    )?;
    let _ = std::fs::remove_dir_all(&root);
    if output.contains("src/main.rs") {
        Ok(())
    } else {
        Err(anyhow!(
            "self-test output did not include src/main.rs: {output}"
        ))
    }
}
