pub(crate) mod change_capture;
pub(super) mod edit_file;
pub(crate) mod file_contents;
pub(super) mod read_tools;
pub(super) mod write_file;

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Serialize;

use super::shared::ToolError;
use crate::tool_call_lifecycle::FailureClass;

pub(super) fn deny_workspace_file_writes() -> Result<(), ToolError> {
    if crate::tool_call_lifecycle::runtime::current_tool_runtime_context()
        .and_then(|scope| scope.workspace_authority)
        .is_some_and(|authority| !authority.allows_file_writes())
    {
        return Err(ToolError::reported_failure(
            FailureClass::PolicyDenied,
            "workspace authority does not allow file writes".into(),
        ));
    }
    Ok(())
}

pub(super) fn merge_optional_notes(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (None, None) => None,
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (Some(a), Some(b)) => Some(format!("{a}\n{b}")),
    }
}

pub(crate) fn content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Language-server resource edits can name binary files; snapshot checks
/// must keep memory bounded.
pub(crate) fn file_content_hash(path: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read as _;
    let mut file = open_regular_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = match file.read(&mut buffer) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}

pub(crate) fn open_regular_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the mutation target is not a regular file",
        ));
    }
    Ok(file)
}

/// Per-path serialization for file mutators (edit_file read → hash-check →
/// match → write; write_file hash-check → overwrite): without it a concurrent
/// in-process mutation can land inside the validated window and be silently
/// overwritten. External writers are outside the lock — they are caught by
/// the expected_content_hash check; there is no OS-level file lock (#724).
static FILE_MUTATION_LOCKS: std::sync::LazyLock<
    std::sync::Mutex<rapidhash::RapidHashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

fn canonical_lock_key(path: &Path) -> PathBuf {
    if let Ok(canonical) = std::fs::canonicalize(path) {
        return canonical;
    }
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    let mut current = path;
    while let Some(parent) = current.parent() {
        match current.file_name() {
            Some(name) => suffix.push(name.to_os_string()),
            None => break,
        }
        if let Ok(canonical) = std::fs::canonicalize(parent) {
            let mut key = canonical;
            for part in suffix.iter().rev() {
                key.push(part);
            }
            return key;
        }
        current = parent;
    }
    path.to_path_buf()
}

pub(crate) fn file_mutation_lock_for(path: &Path) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    let key = canonical_lock_key(path);
    let mut locks = FILE_MUTATION_LOCKS
        .lock()
        .expect("file mutation lock registry poisoned");
    locks.entry(key).or_default().clone()
}

const OUTPUT_META_PREFIX: &str = "gents_fs: ";

pub(super) fn render_tool_output(
    metadata: &impl Serialize,
    body: String,
    raw_value: &impl Serialize,
    raw_json: bool,
) -> Result<String> {
    if raw_json {
        return crate::tool_output::render(
            raw_value,
            &["content", "entries", "matches", "diff", "status", "ok"],
        );
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
