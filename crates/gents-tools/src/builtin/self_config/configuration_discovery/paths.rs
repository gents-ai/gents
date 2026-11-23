use super::*;
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
    }
}

pub(super) fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
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
