//! Grok-side session record for the pager's `/resume` picker.
//!
//! Stock Grok lists whatever the leader returns from `x.ai/session/list`, but
//! it only sends `session/load` for a session that also has a local record at
//! `<grok home>/sessions/<percent-encoded cwd>/<session id>/summary.json`.
//! Grok's own leader writes that record; when Gents is the leader nothing
//! does, so shim sessions are listed but silently unresumable. The shim writes
//! the minimal record Grok needs at `session/new`. The record is Grok's file
//! and Gents never reads it back; the durable session is the `AgentSession`
//! document.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use serde_json::json;

/// `$GROK_HOME` (documented by grok as the config directory override), else
/// `$HOME/.grok`.
pub(super) fn default_grok_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("GROK_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".grok"))
}

/// Grok names the per-cwd session directory with the percent-encoded path:
/// everything but unreserved characters (`A-Z a-z 0-9 - _ . ~`) is `%XX`.
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

/// Session ids come from the pager, so only a plain id may become a path
/// segment inside the operator's grok home.
fn is_plain_segment(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Write the minimal record unless one already exists (Grok owns it after
/// that). Returns the path written, or `None` when nothing was written.
pub(super) fn write_record(
    grok_home: &Path,
    cwd: &str,
    session_id: &str,
) -> Result<Option<PathBuf>> {
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
    fn encodes_the_cwd_the_way_grok_names_session_directories() {
        // Observed on disk: ~/.grok/sessions/%2FUsers%2Fedjroz%2FRepos%2Fgents
        // and %2FRepos%2FSource%2Fdefradb.rs, %2F...%2Fdefradb-rustffi.
        assert_eq!(
            encode_cwd("/Users/edjroz/Repos/gents"),
            "%2FUsers%2Fedjroz%2FRepos%2Fgents"
        );
        assert_eq!(encode_cwd("/Repos/defradb.rs"), "%2FRepos%2Fdefradb.rs");
        assert_eq!(encode_cwd("/a b/x-y_z~"), "%2Fa%20b%2Fx-y_z~");
    }

    #[test]
    fn writes_the_minimal_record_once_and_leaves_an_existing_one_alone() {
        let home = tempfile::tempdir().expect("grok home");
        let written = write_record(home.path(), "/Users/edjroz/Repos/gents", "3e5d627d")
            .expect("write")
            .expect("first write creates the record");
        assert_eq!(
            written,
            home.path()
                .join("sessions/%2FUsers%2Fedjroz%2FRepos%2Fgents/3e5d627d/summary.json")
        );
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&written).expect("read")).expect("json");
        assert_eq!(record["info"]["id"], "3e5d627d");
        assert_eq!(record["info"]["cwd"], "/Users/edjroz/Repos/gents");
        let created = record["created_at"].as_str().expect("created_at");
        assert!(created.ends_with('Z'), "rfc3339 utc: {created}");
        assert_eq!(record["updated_at"], record["created_at"]);
        assert_eq!(record["last_active_at"], record["created_at"]);
        assert_eq!(record["chat_format_version"], 1);

        let again = write_record(home.path(), "/Users/edjroz/Repos/gents", "3e5d627d")
            .expect("second write");
        assert_eq!(again, None, "an existing record belongs to grok");
        let unchanged: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&written).expect("read")).expect("json");
        assert_eq!(unchanged, record);
    }

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
