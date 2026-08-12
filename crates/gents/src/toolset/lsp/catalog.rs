use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULTS_JSON: &str = include_str!("defaults.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogServer {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub file_types: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    #[serde(default)]
    pub is_linter: bool,
    #[serde(default = "default_priority")]
    pub priority: u16,
    #[serde(default)]
    pub language_id: Option<String>,
    #[serde(default)]
    pub init_options: Option<serde_json::Value>,
    #[serde(default)]
    pub settings: Option<serde_json::Value>,
    #[serde(default)]
    pub capabilities: Option<serde_json::Value>,
    #[serde(default)]
    pub workspace_ready_timings: Option<serde_json::Value>,
    #[serde(default)]
    pub warmup_timeout_ms: Option<u64>,
}

fn default_priority() -> u16 {
    50
}

pub fn builtin_catalog() -> Vec<CatalogServer> {
    serde_json::from_str(DEFAULTS_JSON).expect("lsp defaults.json is valid")
}

pub fn marker_matches(root: &Path, markers: &[String]) -> bool {
    markers.iter().any(|marker| {
        if marker.contains('*') {
            if let Ok(entries) = std::fs::read_dir(root) {
                let suffix = marker.trim_start_matches('*');
                return entries.flatten().any(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(suffix.trim_start_matches('.'))
                        || glob_one_level(root, marker)
                });
            }
            false
        } else {
            root.join(marker).exists()
        }
    })
}

fn glob_one_level(root: &Path, pattern: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    let suffix = pattern.rsplit_once('*').map(|(_, s)| s).unwrap_or("");
    entries
        .flatten()
        .any(|entry| entry.file_name().to_string_lossy().ends_with(suffix))
}

pub fn detect_admitted_servers(workspace: &Path, servers: &[CatalogServer]) -> Vec<CatalogServer> {
    servers
        .iter()
        .filter(|server| marker_matches(workspace, &server.root_markers))
        .filter(|server| family_eligible(server, workspace))
        .filter(|server| super::admit::admit_command(&server.command, workspace).is_ok())
        .cloned()
        .collect()
}

pub fn family_eligible(server: &CatalogServer, root: &Path) -> bool {
    match server.name.as_str() {
        "typescript-language-server" => !marker_matches(
            root,
            &["deno.json".into(), "deno.jsonc".into(), "deno.lock".into()],
        ),
        "denols" => marker_matches(root, &server.root_markers),
        _ => true,
    }
}

pub fn primary_for_file<'a>(
    servers: &'a [CatalogServer],
    file: &Path,
) -> Option<&'a CatalogServer> {
    let ext = file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let mut matches: Vec<&CatalogServer> = servers
        .iter()
        .filter(|s| !s.is_linter)
        .filter(|s| {
            s.file_types
                .iter()
                .any(|ft| ft.eq_ignore_ascii_case(&ext) || ft.eq_ignore_ascii_case(&name))
        })
        .collect();
    matches.sort_by_key(|s| (s.priority, s.name.as_str()));
    matches.first().copied()
}

pub fn file_type_matches(server: &CatalogServer, file: &Path) -> bool {
    let ext = file
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    server
        .file_types
        .iter()
        .any(|ft| ft.eq_ignore_ascii_case(&ext) || ft.eq_ignore_ascii_case(&name))
}

/// LSP `languageId` for `textDocument/didOpen`. Extension fallback is not
/// the same as the protocol id (`rs` is not `rust`).
pub fn language_id_for_path(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => "rust",
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "py" | "pyi" => "python",
        "rb" | "rake" | "gemspec" | "erb" => "ruby",
        "ex" | "exs" => "elixir",
        "heex" | "eex" => "phoenix-heex",
        "nix" => "nix",
        "php" | "phtml" => "php",
        "swift" => "swift",
        "json" => "json",
        other if !other.is_empty() => other,
        _ => "plaintext",
    }
    .to_string()
}
