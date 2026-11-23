#[test]
fn native_reset_acknowledges_clearing_an_isolated_datastore() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("browser-clear.json");
    let identifier = objc2_foundation::NSUUID::UUID()
        .UUIDString()
        .to_string()
        .replace('-', "");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gents-desktop-tauri"))
        .arg("__verify-browser-clear")
        .arg("--webview-store-id")
        .arg(identifier)
        .arg(&receipt)
        .status()
        .unwrap();
    assert!(status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(value, serde_json::json!({"completed": true}));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&receipt).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn native_reset_never_overwrites_an_existing_receipt() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("browser-clear.json");
    std::fs::write(&receipt, b"existing receipt").unwrap();
    let identifier = objc2_foundation::NSUUID::UUID()
        .UUIDString()
        .to_string()
        .replace('-', "");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gents-desktop-tauri"))
        .arg("__clear-browser-data")
        .arg("--webview-store-id")
        .arg(identifier)
        .arg(&receipt)
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(std::fs::read(&receipt).unwrap(), b"existing receipt");
}

#[test]
fn malformed_isolation_never_falls_back_to_default_user_data() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("browser-clear.json");
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_gents-desktop-tauri"))
        .arg("__clear-browser-data")
        .arg("--webview-store-id")
        .arg("invalid")
        .arg(&receipt)
        .status()
        .unwrap();
    assert!(!status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(value["completed"], false);
    assert!(value["error"].as_str().unwrap().contains("32 hex digits"));
}

#[test]
fn browser_reset_preflight_accepts_the_cli_check_command() {
    let root = tempfile::tempdir().unwrap();
    let identifier = objc2_foundation::NSUUID::UUID()
        .UUIDString()
        .to_string()
        .replace('-', "")
        .to_lowercase();
    let receipt = root.path().join("browser-check.json");
    let held = gents_desktop_core::home_reset::lock_browser_store(Some(&identifier)).unwrap();
    let check = |receipt: &std::path::Path| {
        std::process::Command::new(env!("CARGO_BIN_EXE_gents-desktop-tauri"))
            .arg("__clear-browser-data")
            .arg("--webview-store-id")
            .arg(&identifier)
            .arg("--check")
            .arg(receipt)
            .output()
            .unwrap()
    };
    let refused = check(&receipt);
    assert!(!refused.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(value["completed"], false);
    assert!(value["error"]
        .as_str()
        .is_some_and(|error| !error.is_empty()));
    drop(held);

    let receipt = root.path().join("browser-check-after-release.json");
    let accepted = check(&receipt);
    assert!(
        accepted.status.success(),
        "{}",
        String::from_utf8_lossy(&accepted.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(value, serde_json::json!({"completed": true}));
}
