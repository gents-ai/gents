use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_INSTRUCTION_PATHS: &[&str] = &["AGENTS.override.md", "AGENTS.md"];
/// Bound workspace-supplied instructions so they cannot consume the entire
/// provider context before the actual request is assembled.
pub const INSTRUCTION_TEXT_BUDGET: usize = 32 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstructionManifest {
    pub schema: u32,
    pub base_sha: String,
    pub files: Vec<InstructionFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstructionFile {
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub truncated: String,
    pub text: String,
}

impl InstructionManifest {
    pub fn new(base_sha: impl Into<String>, files: Vec<InstructionFile>) -> Self {
        Self {
            schema: 1,
            base_sha: base_sha.into(),
            files,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw == "{}" {
            return None;
        }
        serde_json::from_str(raw).ok()
    }

    pub fn to_json_string(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

impl InstructionFile {
    pub fn from_bytes(path: &str, bytes: &[u8]) -> Self {
        Self::from_bytes_with_budget(path, bytes, INSTRUCTION_TEXT_BUDGET)
    }

    fn from_bytes_with_budget(path: &str, bytes: &[u8], budget: usize) -> Self {
        let digest = Sha256::digest(bytes);
        let sha256 = format!("{digest:x}");
        let (text, truncated) = bound_instruction_text_to(bytes, budget);
        Self {
            path: path.to_string(),
            sha256,
            bytes: bytes.len(),
            truncated,
            text,
        }
    }
}

pub fn is_empty_manifest(raw: &str) -> bool {
    match InstructionManifest::parse(raw) {
        Some(manifest) => manifest.is_empty(),
        None => true,
    }
}

pub fn instruction_context_section(raw: &str) -> Option<String> {
    let manifest = InstructionManifest::parse(raw)?;
    if manifest.is_empty() {
        return None;
    }
    Some(format_instruction_section(
        &format!(
            "# Frozen workspace instructions (base_sha={})",
            manifest.base_sha
        ),
        &manifest.files,
    ))
}

pub fn live_instruction_context_section(
    cwd: Option<&Path>,
    tool_root: Option<&Path>,
) -> Option<String> {
    let tool_root = tool_root?;
    let files = discover_live_instruction_files(cwd, tool_root);
    if files.is_empty() {
        return None;
    }
    Some(format_instruction_section(
        "# Live workspace instructions",
        &files,
    ))
}

pub fn instruction_body_for_request(
    frozen_instruction_manifest: Option<&str>,
    live_cwd: Option<&Path>,
    live_tool_root: Option<&Path>,
) -> Option<String> {
    match frozen_instruction_manifest {
        Some(raw) => instruction_context_section(raw),
        None => live_instruction_context_section(live_cwd, live_tool_root),
    }
}

fn format_instruction_section(header: &str, files: &[InstructionFile]) -> String {
    let mut body = String::from(header);
    body.push('\n');
    for file in files {
        body.push('\n');
        body.push_str("## ");
        body.push_str(&file.path);
        body.push('\n');
        if !file.truncated.is_empty() {
            body.push_str(&file.truncated);
            body.push('\n');
        }
        body.push_str(&file.text);
        if !file.text.ends_with('\n') {
            body.push('\n');
        }
    }
    body
}

fn discover_live_instruction_files(cwd: Option<&Path>, tool_root: &Path) -> Vec<InstructionFile> {
    let Ok(tool_root) = std::fs::canonicalize(tool_root) else {
        return Vec::new();
    };
    if !tool_root.is_dir() {
        return Vec::new();
    }
    let start = cwd
        .and_then(|path| std::fs::canonicalize(path).ok())
        .filter(|path| path.is_dir() && path.starts_with(&tool_root))
        .unwrap_or_else(|| tool_root.clone());

    let mut dirs = Vec::new();
    let mut current = start;
    loop {
        dirs.push(current.clone());
        if current == tool_root {
            break;
        }
        match current.parent() {
            Some(parent) => current = parent.to_path_buf(),
            None => break,
        }
    }
    dirs.reverse();

    let mut remaining = INSTRUCTION_TEXT_BUDGET;
    let mut files = Vec::new();
    for dir in dirs {
        if remaining == 0 {
            break;
        }
        let Some(found) = first_live_instruction_file(&dir, &tool_root) else {
            continue;
        };
        let Some(bytes) = read_live_instruction_prefix(&found.read_path, remaining) else {
            continue;
        };
        let file = InstructionFile::from_bytes_with_budget(&found.display, &bytes, remaining);
        if file.text.is_empty() {
            continue;
        }
        if !file.truncated.is_empty() {
            tracing::warn!(
                path = %found.display,
                budget = remaining,
                "live AGENTS.md truncated to instruction budget"
            );
        }
        remaining = remaining.saturating_sub(file.text.len());
        files.push(file);
    }
    files
}

struct LiveInstructionPath {
    display: String,
    read_path: PathBuf,
}

fn first_live_instruction_file(dir: &Path, tool_root: &Path) -> Option<LiveInstructionPath> {
    for name in DEFAULT_INSTRUCTION_PATHS {
        let candidate = dir.join(name);
        let Ok(canonical) = std::fs::canonicalize(&candidate) else {
            continue;
        };
        if !canonical.is_file() || !canonical.starts_with(tool_root) {
            continue;
        }
        match std::fs::metadata(&canonical) {
            Ok(meta) if meta.len() > 0 => {
                return Some(LiveInstructionPath {
                    display: instruction_display_path(&candidate, tool_root),
                    read_path: canonical,
                });
            }
            _ => continue,
        }
    }
    None
}

fn instruction_display_path(path: &Path, tool_root: &Path) -> String {
    path.strip_prefix(tool_root)
        .ok()
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .filter(|relative| !relative.is_empty())
        .or_else(|| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "AGENTS.md".to_string())
}

fn read_live_instruction_prefix(path: &Path, remaining: usize) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    let cap = remaining.saturating_add(4);
    file.take(cap as u64).read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

fn bound_instruction_text_to(bytes: &[u8], budget: usize) -> (String, String) {
    let lossy = String::from_utf8_lossy(bytes);
    if budget == 0 {
        return (String::new(), "truncated to 0 bytes".to_string());
    }
    if lossy.len() <= budget {
        return (lossy.into_owned(), String::new());
    }
    let mut end = budget;
    while end > 0 && !lossy.is_char_boundary(end) {
        end -= 1;
    }
    (
        lossy[..end].to_string(),
        format!("truncated to {end} bytes"),
    )
}
