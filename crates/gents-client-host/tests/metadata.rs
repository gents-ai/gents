use std::process::Command;

use crate::support::homes;

#[test]
fn artifact_description_does_not_open_a_client_store() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("unopened-client");
    let homes = homes::ProcessHomes::under(temp.path());
    let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .arg("--describe")
        .envs(homes.env())
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
        .envs(homes::ProcessHomes::under(temp.path()).env())
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
        .envs(homes::ProcessHomes::under(temp.path()).env())
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
    let homes = homes::ProcessHomes::under(temp.path());
    for role in ["--serve", "--stdio", "--describe", "--editor-store"] {
        let output = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
            .envs(homes.env())
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

#[test]
fn runtime_key_unlock_preserves_existing_keys_and_refuses_a_missing_key() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let process = tempfile::tempdir().unwrap();
    let homes = homes::ProcessHomes::under(process.path());
    let executable = env!("CARGO_BIN_EXE_gents-client-host");
    let initialized = Command::new(executable)
        .envs(homes.env())
        .args([
            "init",
            "--identity-only",
            "--store-key-custody",
            "file",
            "--tool-package",
            "minimal",
            "--home",
        ])
        .arg(&home)
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let config_path = home.join("init.json");
    let config_bytes = std::fs::read(&config_path).unwrap();
    let config: serde_json::Value = serde_json::from_slice(&config_bytes).unwrap();
    let identity_path = std::path::Path::new(config["key_path"].as_str().unwrap());
    let encryption_path = gents_store::local::store_key::home_key_file(&home);
    let identity = std::fs::read(identity_path).unwrap();
    let encryption = std::fs::read(&encryption_path).unwrap();
    let unlock = || {
        Command::new(executable)
            .envs(homes.env())
            .arg("--unlock-runtime-keys")
            .arg(&home)
            .output()
            .unwrap()
    };
    let output = unlock();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(std::fs::read(identity_path).unwrap() == identity);
    assert!(std::fs::read(&encryption_path).unwrap() == encryption);
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);

    // Identity-only init bootstraps the owner into a fresh protected store; refusing the
    // missing key must leave that store byte-for-byte as init wrote it.
    let data = gents_store::local::home::default_data_dir(&home);
    assert!(data.join("MANIFEST").exists());
    let store = store_files(&data);
    std::fs::remove_file(&encryption_path).unwrap();
    let output = unlock();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!encryption_path.exists());
    assert!(std::fs::read(identity_path).unwrap() == identity);
    assert_eq!(std::fs::read(&config_path).unwrap(), config_bytes);
    assert!(
        store_files(&data) == store,
        "the refused unlock changed the store"
    );
    assert!(!home.join("runtime.json").exists());
}

fn store_files(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let contents = std::fs::read(&path).unwrap();
                files.push((path.strip_prefix(root).unwrap().to_path_buf(), contents));
            }
        }
    }
    files.sort();
    files
}
