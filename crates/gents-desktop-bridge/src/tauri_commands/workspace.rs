use crate::error::BridgeError;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::state::{current_core, require_agent_home, DesktopAppState};

const MAX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceEntryView {
    pub name: String,
    #[ts(type = "\"dir\" | \"file\"")]
    pub kind: &'static str,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListingView {
    pub root: String,
    pub subpath: String,
    pub entries: Vec<WorkspaceEntryView>,
    pub truncated: bool,
}

/// List one directory of the local agent's tool root. Strictly jailed:
/// the canonicalized target must stay under the canonicalized root, so
/// `..` segments and symlink escapes both fail closed.
#[tauri::command]
pub fn desktop_workspace_list(
    subpath: Option<String>,
    state: State<'_, DesktopAppState>,
) -> Result<WorkspaceListingView, BridgeError> {
    if current_core(&state).is_none() {
        return Err(BridgeError::from_legacy_message(
            "desktop client is not running",
        ));
    }

    let root = workspace_root(&state)?;
    let subpath = subpath.unwrap_or_default();
    let requested = if subpath.trim().is_empty() {
        root.clone()
    } else {
        root.join(subpath.trim_matches('/'))
    };
    let target = requested.canonicalize().map_err(|error| {
        BridgeError::from_legacy_message(format!("cannot open {}: {error}", requested.display()))
    })?;
    if !target.starts_with(&root) {
        return Err(BridgeError::from_legacy_message(
            "path escapes the workspace root",
        ));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    let read_dir = std::fs::read_dir(&target).map_err(|error| {
        BridgeError::from_legacy_message(format!("cannot list {}: {error}", target.display()))
    })?;
    for entry in read_dir.flatten() {
        if entries.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_dir() { "dir" } else { "file" };
        let size = (kind == "file")
            .then(|| entry.metadata().ok().map(|meta| meta.len()))
            .flatten();
        entries.push(WorkspaceEntryView { name, kind, size });
    }
    entries.sort_by(|left, right| {
        (left.kind != "dir", left.name.to_lowercase())
            .cmp(&(right.kind != "dir", right.name.to_lowercase()))
    });

    let display_subpath = target
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(WorkspaceListingView {
        root: root.display().to_string(),
        subpath: display_subpath,
        entries,
        truncated,
    })
}

/// The agent home's configured tool root, canonicalized. No tool root
/// (meta-only agents) is an honest error, not an empty listing.
fn workspace_root(state: &State<'_, DesktopAppState>) -> Result<PathBuf, BridgeError> {
    #[derive(serde::Deserialize)]
    struct InitRootView {
        #[serde(default)]
        tool_root: Option<String>,
    }

    let agent_home = require_agent_home(state)?;
    let root = std::fs::read(agent_home.join("init.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<InitRootView>(&bytes).ok())
        .and_then(|config| config.tool_root)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            BridgeError::from_legacy_message(
                "this agent has no tool root configured (meta-only ceiling)",
            )
        })?;
    Path::new(&root).canonicalize().map_err(|error| {
        BridgeError::from_legacy_message(format!("tool root {root} is not accessible: {error}"))
    })
}
