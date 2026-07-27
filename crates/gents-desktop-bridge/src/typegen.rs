//! Phase-2 type-generation spike and bindings freshness gate.
//!
//! Decision: **ts-rs** (not typeshare).
//! Evidence:
//! - `serde-compat` honors `rename_all = "camelCase"` and tagged enums
//!   (`RenderedTimelineItem` with `tag = "kind"`) without extra attributes.
//! - Nested structs (`ToolServiceTestResult` → `ToolServiceToolView`) export
//!   transitively.
//! - `#[ts(type = "string")]` covers `&'static str` fields (`ClientUpdateEvent`).
//! - typeshare would require a separate CLI + attribute surface and has weaker
//!   serde-attribute coverage for our existing view models.
//!
//! Generated files land under `crates/gents-desktop-bridge/bindings/` (committed).
//! Phase 5 moves them into `@source-inc/gents-desktop-client/src/generated/`.

use std::path::{Path, PathBuf};

use ts_rs::TS;

use crate::error::{BridgeError, BridgeErrorCode};
use crate::types::{
    ChatSendResult, ClientUpdateEvent, DesktopBootstrapSummary, RenderedTimelineItem,
    SavedPeerView, ToolServiceTestResult,
};

fn bindings_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bindings")
}

fn export_all(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // export_all_to writes each type (and dependencies) as a .ts file.
    BridgeErrorCode::export_all_to(dir).map_err(|e| e.to_string())?;
    BridgeError::export_all_to(dir).map_err(|e| e.to_string())?;
    SavedPeerView::export_all_to(dir).map_err(|e| e.to_string())?;
    DesktopBootstrapSummary::export_all_to(dir).map_err(|e| e.to_string())?;
    ChatSendResult::export_all_to(dir).map_err(|e| e.to_string())?;
    ClientUpdateEvent::export_all_to(dir).map_err(|e| e.to_string())?;
    ToolServiceTestResult::export_all_to(dir).map_err(|e| e.to_string())?;
    RenderedTimelineItem::export_all_to(dir).map_err(|e| e.to_string())?;
    Ok(())
}

fn list_ts_files(dir: &Path) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    if !dir.exists() {
        return Ok(names);
    }
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("ts") {
            names.push(path.file_name().unwrap().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

#[test]
fn ts_rs_exports_tagged_enum_and_camel_case_structs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let timeline = std::fs::read_to_string(tmp.path().join("RenderedTimelineItem.ts"))
        .expect("RenderedTimelineItem.ts");
    assert!(
        timeline.contains("kind") && timeline.contains("userMessage"),
        "tagged enum should surface the serde tag + camelCase variant; got:\n{timeline}"
    );
    assert!(
        timeline.contains("itemKey"),
        "per-variant rename_all should camelCase enum fields; got:\n{timeline}"
    );

    let bootstrap = std::fs::read_to_string(tmp.path().join("DesktopBootstrapSummary.ts"))
        .expect("DesktopBootstrapSummary.ts");
    assert!(
        bootstrap.contains("defaultAgentHome"),
        "serde rename_all camelCase should be reflected; got:\n{bootstrap}"
    );

    let error = std::fs::read_to_string(tmp.path().join("BridgeError.ts")).expect("BridgeError.ts");
    assert!(
        error.contains("retryable"),
        "BridgeError fields; got:\n{error}"
    );
}

#[test]
fn committed_bindings_match_regeneration() {
    let committed = bindings_dir();
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let expected_files = list_ts_files(tmp.path()).expect("list generated");
    let actual_files = list_ts_files(&committed).unwrap_or_default();
    assert_eq!(
        actual_files, expected_files,
        "bindings file set drifted. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored"
    );

    for name in &expected_files {
        let expected = std::fs::read_to_string(tmp.path().join(name)).expect("read generated");
        let actual = std::fs::read_to_string(committed.join(name)).unwrap_or_else(|_| {
            panic!("missing committed binding {name}; regenerate with write_bindings")
        });
        assert_eq!(
            actual, expected,
            "binding {name} drifted. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored"
        );
    }
}

#[test]
#[ignore = "run explicitly to regenerate crates/gents-desktop-bridge/bindings/"]
fn write_bindings() {
    let dir = bindings_dir();
    // Clear prior exports so renames don't leave stale files.
    if dir.exists() {
        for entry in std::fs::read_dir(&dir).expect("read bindings") {
            let entry = entry.expect("entry");
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("ts") {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    export_all(&dir).expect("export bindings");
    eprintln!("wrote bindings to {}", dir.display());
}
