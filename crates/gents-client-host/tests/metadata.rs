use std::process::Command;

#[test]
fn artifact_description_does_not_open_a_client_store() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("unopened-client");
    let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--describe")
        .env("GENTS_DESKTOP_HOME", &home)
        .env("GENTS_HOME", &home)
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(metadata["name"], "gents-client-host");
    assert_eq!(metadata["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        metadata["protocolVersion"],
        gents_desktop_bridge::host::protocol::PROTOCOL_VERSION
    );
    assert_eq!(
        metadata["editorStoreBootstrapVersion"],
        gents_desktop_bridge::host::protocol::EDITOR_STORE_BOOTSTRAP_VERSION
    );
    assert_eq!(
        metadata["commands"],
        serde_json::json!(gents_desktop_bridge::host::protocol::ClientCommand::NAMES)
    );
    assert_eq!(metadata["transportSupported"], cfg!(any(unix, windows)));
    assert_eq!(metadata["runtimeKeyUnlock"], true);
    assert_eq!(metadata["editorCheckpointVersion"], 1);
    assert_eq!(metadata["editorCheckpointSupported"], cfg!(unix));
    assert!(output.stderr.is_empty());
    assert!(!home.exists());
}

#[test]
fn artifact_description_cannot_be_combined_with_a_store_operation() {
    let temp = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--describe", "--home"])
        .arg(temp.path().join("unopened-client"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!temp.path().join("unopened-client").exists());
}

#[test]
fn runtime_key_unlock_does_not_initialize_a_missing_home() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("missing-runtime");
    let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--unlock-runtime-keys")
        .arg(&home)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!home.exists());
}

#[test]
fn runtime_key_unlock_cannot_be_combined_with_another_helper_role() {
    let temp = tempfile::tempdir().unwrap();
    for role in ["--serve", "--stdio", "--describe", "--editor-store"] {
        let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .arg("--unlock-runtime-keys")
            .arg(temp.path().join("missing-runtime"))
            .arg(role)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{role}");
        assert!(output.stdout.is_empty(), "{role}");
        assert!(!temp.path().join("missing-runtime").exists());
    }
}
