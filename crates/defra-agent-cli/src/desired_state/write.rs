use std::path::Path;

use super::DesiredStateManifest;

/// Verify that `id` is a valid per-document directory handle. Rejects any
/// character that would break filesystem semantics or produce ambiguous
/// paths (`/`, `\`, `:`, null byte), the traversal specials `.` and
/// `..`, and the empty string.
pub(crate) fn check_filesystem_safe_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err(
            "unique id is empty; choose a filesystem-safe id".to_string(),
        );
    }
    if id == "." || id == ".." {
        return Err(format!(
            "unique id '{id}' contains filesystem-unsafe value; choose a filesystem-safe id"
        ));
    }
    for ch in id.chars() {
        if matches!(ch, '/' | '\\' | ':' | '\0') {
            return Err(format!(
                "unique id '{id}' contains filesystem-unsafe character(s); choose a filesystem-safe id"
            ));
        }
    }
    Ok(())
}

/// Write a `DesiredStateManifest` to `root` as a manifest root directory.
/// See `docs/superpowers/specs/2026-04-22-per-agent-manifest-roots-design.md`
/// for the on-disk layout contract.
// Implemented in Task 7 (per-agent manifest roots, #67).
#[allow(dead_code)]
pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<(), String> {
    let _ = (root, manifest, force);
    unimplemented!("implemented in Task 7")
}
