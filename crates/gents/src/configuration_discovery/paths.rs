use super::*;
use std::fs::{self, Metadata};
use std::path::{Component, Path, PathBuf};

pub(super) fn layout(
    source: &DiscoverySourceRoot,
) -> (Vec<(PathBuf, ConfigFormat)>, Vec<PathBuf>, Vec<PathBuf>) {
    match (source.kind, source.scope) {
        (DiscoverySourceKind::Claude, DiscoveryScope::User) => (
            vec![
                ("settings.json".into(), ConfigFormat::ClaudeJson),
                ("settings.local.json".into(), ConfigFormat::ClaudeJson),
                ("mcp.json".into(), ConfigFormat::McpJson),
            ],
            vec!["CLAUDE.md".into()],
            vec!["skills".into()],
        ),
        (DiscoverySourceKind::Claude, DiscoveryScope::Project) => (
            vec![
                (".claude/settings.json".into(), ConfigFormat::ClaudeJson),
                (
                    ".claude/settings.local.json".into(),
                    ConfigFormat::ClaudeJson,
                ),
                (".mcp.json".into(), ConfigFormat::McpJson),
                (".claude/.mcp.json".into(), ConfigFormat::McpJson),
                (".claude/mcp.json".into(), ConfigFormat::McpJson),
            ],
            vec!["CLAUDE.md".into(), ".claude/CLAUDE.md".into()],
            vec![".claude/skills".into()],
        ),
        (DiscoverySourceKind::Codex, DiscoveryScope::User) => (
            vec![("config.toml".into(), ConfigFormat::CodexToml)],
            vec!["AGENTS.md".into()],
            vec!["skills".into()],
        ),
        (DiscoverySourceKind::Codex, DiscoveryScope::Project) => (
            vec![(".codex/config.toml".into(), ConfigFormat::CodexToml)],
            vec!["AGENTS.md".into()],
            vec![".codex/skills".into()],
        ),
        (DiscoverySourceKind::Grok, DiscoveryScope::User) => (
            vec![
                ("config.toml".into(), ConfigFormat::GrokToml),
                ("mcp.json".into(), ConfigFormat::McpJson),
            ],
            vec!["AGENTS.md".into()],
            vec!["skills".into()],
        ),
        (DiscoverySourceKind::Grok, DiscoveryScope::Project) => (
            vec![
                (".grok/config.toml".into(), ConfigFormat::GrokToml),
                (".mcp.json".into(), ConfigFormat::McpJson),
            ],
            vec!["AGENTS.md".into()],
            vec![".grok/skills".into()],
        ),
    }
}

#[derive(Debug)]
pub(super) enum SafePathError {
    Symlink,
    Io,
}

pub(super) fn reject_symlink_components(
    root: &Path,
    relative: &Path,
) -> Result<Option<Metadata>, SafePathError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(SafePathError::Io);
        };
        current.push(component);
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(SafePathError::Io),
        };
        if metadata.file_type().is_symlink() {
            return Err(SafePathError::Symlink);
        }
    }
    fs::symlink_metadata(current)
        .map(Some)
        .map_err(|_| SafePathError::Io)
}

pub(super) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

pub(super) fn source_root_unchanged(source: &SourceScan) -> bool {
    fs::symlink_metadata(&source.root).is_ok_and(|current| {
        current.is_dir()
            && !current.file_type().is_symlink()
            && metadata_same_file(&source.root_metadata, &current)
    })
}

#[cfg(unix)]
pub(super) fn metadata_unchanged(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.len() == after.len()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
pub(super) fn metadata_unchanged(before: &Metadata, after: &Metadata) -> bool {
    before.len() == after.len() && before.modified().ok() == after.modified().ok()
}

#[cfg(unix)]
pub(super) fn metadata_same_file(before: &Metadata, after: &Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev() && before.ino() == after.ino() && metadata_unchanged(before, after)
}

#[cfg(not(unix))]
pub(super) fn metadata_same_file(before: &Metadata, after: &Metadata) -> bool {
    metadata_unchanged(before, after)
}

pub(super) fn valid_source_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

pub(super) fn path_string(path: &Path) -> String {
    sanitize_text(&raw_path_string(path))
}

pub(super) fn raw_path_string(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
pub(super) type PreopenHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    pub(super) static PREOPEN_TEST_HOOK: std::cell::RefCell<Option<PreopenHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn run_preopen_test_hook(path: &Path) {
    PREOPEN_TEST_HOOK.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook(path);
        }
    });
}

#[cfg(not(test))]
pub(super) fn run_preopen_test_hook(_: &Path) {}
