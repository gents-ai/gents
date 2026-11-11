use std::path::Path;

use gents_shell::keys::KeySet;
use serde_json::{json, Value};

/// The shell's own client-local choices.
fn keys_path(home: &Path) -> std::path::PathBuf {
    home.join(gents_store::local::home::SHELL_CONFIG_FILE_NAME)
}

/// The saved key set, or Regular when the file is missing, unreadable or invalid.
pub(super) fn read_keys(home: &Path) -> KeySet {
    let saved = std::fs::read(keys_path(home))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    match saved.as_ref().and_then(|saved| saved["keys"].as_str()) {
        Some("vim") => KeySet::Vim,
        Some("emacs") => KeySet::Emacs,
        _ => KeySet::Regular,
    }
}

pub(super) fn save_keys(home: &Path, keys: KeySet) -> std::io::Result<()> {
    let contents = json!({ "keys": keys.name() }).to_string();
    crate::native_service::atomic_write(&keys_path(home), contents.as_bytes())
        .map_err(|error| std::io::Error::other(format!("{error:#}")))
}
