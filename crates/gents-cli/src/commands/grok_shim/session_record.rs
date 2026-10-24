//! Grok-side session record for the pager's `/resume` picker: stock Grok only
//! sends `session/load` for a session that also has a local record under
//! `<grok home>/sessions/<percent-encoded cwd>/<session id>/summary.json`, so
//! the shim writes the minimal record at `session/new`. The record is Grok's
//! file and Gents never reads it back; the durable session is the
//! `AgentSession` document.

use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};
use serde_json::json;

use crate::request_helpers::RequestHelpersError;

/// `$GROK_HOME` (config-directory override), else `$HOME/.grok`.
pub(super) fn default_grok_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".grok"))
}

/// Grok names the per-cwd session directory with the percent-encoded path
/// (RFC 3986 unreserved characters stay literal).
pub(super) fn encode_cwd(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len() * 3);
    for byte in cwd.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Only a plain id may become a path segment inside the operator's grok home.
fn is_plain_segment(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Returns the path written, or `None` when one already exists (Grok owns it
/// after that).
pub(super) fn write_record(
    grok_home: &Path,
    cwd: &str,
    session_id: &str,
) -> Result<Option<PathBuf>, RequestHelpersError> {
    if !Path::new(cwd).is_absolute() || !is_plain_segment(session_id) {
        return Ok(None);
    }
    let path = grok_home
        .join("sessions")
        .join(encode_cwd(cwd))
        .join(session_id)
        .join("summary.json");
    if path.exists() {
        return Ok(None);
    }
    let stamp = Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true);
    crate::request_helpers::write_json_output_file(
        &path,
        &json!({
            "info": {"id": session_id, "cwd": cwd},
            "created_at": stamp,
            "updated_at": stamp,
            "last_active_at": stamp,
            "chat_format_version": 1,
        }),
    )?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_relative_cwds_and_unsafe_session_ids() {
        let home = tempfile::tempdir().expect("grok home");
        assert_eq!(
            write_record(home.path(), "relative", "s1").expect("ok"),
            None
        );
        assert_eq!(
            write_record(home.path(), "/tmp", "../escape").expect("ok"),
            None
        );
        assert_eq!(write_record(home.path(), "/tmp", "").expect("ok"), None);
        assert!(!home.path().join("sessions").exists());
    }
}
