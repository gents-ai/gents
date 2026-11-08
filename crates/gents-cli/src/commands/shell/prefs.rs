use std::path::Path;

use gents_shell::keys::KeySet;
use serde_json::{json, Value};

/// The shell's own client-local choices, apart from `ClientSettings` which rejects unknown fields.
fn keys_path(home: &Path) -> std::path::PathBuf {
    home.join("shell.json")
}

/// The saved key set, or Vim when the file is missing, unreadable or invalid.
pub(super) fn read_keys(home: &Path) -> KeySet {
    let saved = std::fs::read(keys_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match saved.as_ref().and_then(|saved| saved["keys"].as_str()) {
        Some("emacs") => KeySet::Emacs,
        _ => KeySet::Vim,
    }
}

pub(super) fn save_keys(home: &Path, keys: KeySet) -> std::io::Result<()> {
    let keys = match keys {
        KeySet::Vim => "vim",
        KeySet::Emacs => "emacs",
    };
    let contents = json!({ "keys": keys }).to_string();
    crate::native_service::atomic_write(&keys_path(home), contents.as_bytes())
        .map_err(|error| std::io::Error::other(format!("{error:#}")))
}
