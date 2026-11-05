#![cfg(all(target_os = "macos", feature = "native-e2e", debug_assertions))]

/// A plugin page's sandboxed frame reaches none of the app's IPC routes in
/// the real webview, while the page around it does (#3483).
#[test]
fn a_sandboxed_plugin_frame_cannot_reach_the_ipc() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("frame-ipc.json");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gents-desktop-tauri"))
        .arg("__verify-frame-ipc")
        .arg(&receipt)
        .status()
        .unwrap();
    assert!(status.success());
    let seen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(
        seen["parentReached"], true,
        "the probe page must reach the IPC: {seen}"
    );
    let report = &seen["report"];
    assert!(report.is_object(), "the frame must report back: {seen}");
    assert_eq!(report["origin"], "null", "{seen}");
    assert_eq!(report["internals"], "absent", "{seen}");
    assert_eq!(
        report["parentInternals"]
            .as_str()
            .map(|r| r.starts_with("refused")),
        Some(true),
        "{seen}"
    );
    assert_eq!(seen["frameReached"], serde_json::json!([]), "{seen}");
}
