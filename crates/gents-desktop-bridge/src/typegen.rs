//! TypeScript bridge-contract generation and bindings freshness gate.
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
//! Every public request and serialized view is exported. Generated files land
//! in both `crates/gents-desktop-bridge/bindings/` and
//! `@source-inc/gents-desktop-client/src/generated/` (both committed).

use std::path::{Path, PathBuf};

use ts_rs::TS;

use crate::contract::BridgeContract;
use crate::error::{BridgeError, BridgeErrorCode};
use crate::tauri_commands::chat::{RequestResendResultView, SessionForkResultView};
use crate::tauri_commands::lifecycle::DesktopObserverMetrics;
use crate::tauri_commands::workspace::WorkspaceListingView;
use crate::types::*;

fn bindings_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bindings")
}

fn package_bindings_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/gents-desktop-client/src/generated")
}

fn normalize_generated_types(dir: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|s| s.to_str()) != Some("ts") {
            continue;
        }
        let source = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        // Tauri IPC carries serde integers as JavaScript numbers. ts-rs maps
        // Rust's 64-bit integer types to bigint by default, which describes the
        // Rust value range rather than the JSON wire representation.
        let normalized = source
            .replace("bigint", "number")
            .lines()
            .map(|line| {
                let trimmed = line.trim_end();
                if trimmed.starts_with("import type ") && trimmed.ends_with("\";") {
                    format!("{}.js\";", trimmed.trim_end_matches("\";"))
                } else {
                    trimmed.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(path, normalized).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn export_all(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    macro_rules! export_types {
        ($($type:ty),+ $(,)?) => {
            $(
                <$type>::export_all_to(dir).map_err(|e| e.to_string())?;
            )+
        };
    }

    // Requests are independent inputs, so each command request is a root.
    export_types!(
        DesktopInitRequest,
        PeerAddRequest,
        PeerStatusFetchRequest,
        PeerProbeRequest,
        BearerPairingRequest,
        ChatSendRequest,
        ConversationRenameRequest,
        AgentConfigSaveRequest,
        BehaviorSaveRequest,
        SkillDeleteRequest,
        TaskDeleteRequest,
        ScheduleDeleteRequest,
        EventTriggerDeleteRequest,
        BackendDeleteRequest,
        InferenceProfileDeleteRequest,
        ToolSelectionDeleteRequest,
        ToolServiceDeleteRequest,
        BehaviorDeleteRequest,
        BackendSaveRequest,
        InferenceProfileSaveRequest,
        ToolSelectionSaveRequest,
        ToolServiceSaveRequest,
        ToolServiceTestRequest,
        TaskSaveRequest,
        SkillSaveRequest,
        TaskRunRequest,
        ScheduleSaveRequest,
        ScheduleRunRequest,
        EventTriggerSaveRequest,
        DesktopOperationsSnapshotRequest,
        DesktopListSubagentTreeRequest,
        DesktopPreviewInterruptCascadeRequest,
        DesktopListHoldsRequest,
        DesktopResolveHoldRequest,
        DesktopInterruptRequest,
        DesktopProbeMcpServiceRequest,
    );

    // Response/view roots export their nested dependencies transitively.
    export_types!(
        BridgeErrorCode,
        BridgeError,
        DesktopClientSnapshot,
        PeerRemoveResponse,
        BearerPairingResponse,
        NetworkStatusView,
        ToolServiceTestResult,
        TaskRunResult,
        ChatSendResult,
        ClientUpdateEvent,
        DesktopOperationsSnapshot,
        CascadeCancelPreview,
        InterruptRequestResult,
        HeldToolCallView,
        ResolveHoldResult,
        BackendHealthView,
        MCPServiceHealthView,
        McpServiceProbeResult,
        DerivedCancelCauseView,
        DesktopSessionSnapshot,
        MessageView,
        ToolCallView,
        ToolResultView,
        BridgeContract,
        SessionForkResultView,
        RequestResendResultView,
        WorkspaceListingView,
        DesktopObserverMetrics,
    );

    normalize_generated_types(dir)?;
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

    assert!(
        !timeline.contains("bigint"),
        "serde IPC integers must be emitted as JavaScript numbers; got:\n{timeline}"
    );
}

#[test]
fn all_public_bridge_contract_roots_are_generated() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let files = list_ts_files(tmp.path()).expect("list generated");
    assert!(
        files.len() >= 90,
        "expected full request/view coverage, generated only {} files",
        files.len()
    );
    for expected in [
        "DesktopClientSnapshot.ts",
        "DesktopSessionSnapshot.ts",
        "DesktopOperationsSnapshot.ts",
        "BridgeContract.ts",
        "PeerAddRequest.ts",
        "ChatSendRequest.ts",
        "DesktopInterruptRequest.ts",
    ] {
        assert!(
            files.iter().any(|file| file == expected),
            "missing generated bridge contract {expected}"
        );
    }
}

#[test]
fn committed_bindings_match_regeneration() {
    let tmp = tempfile::tempdir().expect("tempdir");
    export_all(tmp.path()).expect("export");

    let expected_files = list_ts_files(tmp.path()).expect("list generated");
    for committed in [bindings_dir(), package_bindings_dir()] {
        let actual_files = list_ts_files(&committed).unwrap_or_default();
        assert_eq!(
            actual_files, expected_files,
            "bindings file set drifted under {}. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored",
            committed.display()
        );

        for name in &expected_files {
            let expected = std::fs::read_to_string(tmp.path().join(name)).expect("read generated");
            let actual = std::fs::read_to_string(committed.join(name)).unwrap_or_else(|_| {
                panic!("missing committed binding {name}; regenerate with write_bindings")
            });
            assert_eq!(
                actual,
                expected,
                "binding {name} drifted under {}. Regenerate with:\n  cargo test -p gents-desktop-bridge write_bindings -- --ignored",
                committed.display()
            );
        }
    }
}

#[test]
#[ignore = "run explicitly to regenerate crates/gents-desktop-bridge/bindings/"]
fn write_bindings() {
    for dir in [bindings_dir(), package_bindings_dir()] {
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
}
