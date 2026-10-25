#![cfg(target_os = "macos")]

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::local;
use gents_desktop_bridge::{BridgeError, BridgeErrorCode};
use gents_desktop_core::client::DesktopPaths;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

#[tokio::test]
async fn a_retained_editor_cache_miss_stops_before_identity_or_store_startup() {
    let directory = tempfile::tempdir().unwrap();
    let paths = retained_history(directory.path().join("editor client")).await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = FramedWrite::new(child.stdin.take().unwrap(), LengthDelimitedCodec::new());
    let mut output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
    let store = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let store: Value = serde_json::from_slice(&store).unwrap();
    assert_eq!(store["kind"], "store");
    assert_eq!(
        store["version"],
        gents_desktop_bridge::host::protocol::EDITOR_STORE_BOOTSTRAP_VERSION
    );
    assert_eq!(store["record"]["custody"], "macos-keychain");
    assert!(local::connect(&paths).await.is_err());
    let original_record = std::fs::read(paths.store_encryption_path()).unwrap();
    let mut second = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut second_input =
        FramedWrite::new(second.stdin.take().unwrap(), LengthDelimitedCodec::new());
    let mut second_output =
        FramedRead::new(second.stdout.take().unwrap(), LengthDelimitedCodec::new());
    input
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind": "use", "bytes": null})).unwrap(),
        ))
        .await
        .unwrap();
    let error = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let error: Value = serde_json::from_slice(&error).unwrap();
    assert_eq!(error["kind"], "error");
    let failure: BridgeError = serde_json::from_value(error["error"].clone()).unwrap();
    assert_eq!(failure.code, BridgeErrorCode::LocalHistoryLocked);
    assert!(!failure.retryable);
    assert!(!paths.identity_key_path().exists());
    assert!(!paths.store_key_path().exists());
    assert!(!paths.node_data_dir().join("MANIFEST").exists());
    assert_eq!(
        std::fs::read(paths.store_encryption_path()).unwrap(),
        original_record
    );
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!status.success());
    let next_store = tokio::time::timeout(Duration::from_secs(30), second_output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next_store: Value = serde_json::from_slice(&next_store).unwrap();
    assert_eq!(next_store, store);
    second_input
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"use", "bytes":null})).unwrap(),
        ))
        .await
        .unwrap();
    let next_error = tokio::time::timeout(Duration::from_secs(30), second_output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let next_error: Value = serde_json::from_slice(&next_error).unwrap();
    assert_eq!(next_error, error);
    assert!(
        !tokio::time::timeout(Duration::from_secs(10), second.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        std::fs::read(paths.store_encryption_path()).unwrap(),
        original_record
    );
}

/// Child::wait closes stdin. Keep it open until the store lock is released so
/// EOF cannot win the SIGTERM race; then close it to release Tokio's blocking
/// stdin read before waiting for the attachment's runtime to exit.
#[tokio::test]
async fn terminating_an_editor_attachment_releases_its_unready_installation() {
    let directory = tempfile::tempdir().unwrap();
    let paths = retained_history(directory.path().join("cancelled editor")).await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let input = child.stdin.take().unwrap();
    let mut output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
    let first = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["kind"], "store");
    // SAFETY: this is the live attachment created above; it installed its
    // termination handler before requesting the private store exchange.
    assert_eq!(
        unsafe { libc::kill(child.id().unwrap() as libc::pid_t, libc::SIGTERM) },
        0
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match gents::home::lock_store(paths.root(), paths.node_data_dir()) {
                Ok(guard) => {
                    drop(guard);
                    break;
                }
                Err(error) if error.is::<gents::home::StoreLockHeld>() => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("unexpected client lock error: {error}"),
            }
        }
    })
    .await
    .expect("the cancelled startup retained its store lock");
    drop(input);
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(status.success(), "unexpected attachment exit: {status}");
    assert!(!paths.identity_key_path().exists());
    assert!(!paths.node_data_dir().join("MANIFEST").exists());
}

#[tokio::test]
async fn closing_editor_input_releases_an_unready_installation() {
    use tokio::io::AsyncReadExt;

    let directory = tempfile::tempdir().unwrap();
    let paths = retained_history(directory.path().join("closed editor")).await;
    let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
    let first = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let first: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(first["kind"], "store");
    drop(child.stdin.take());
    let status = tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap();
    let mut error = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        child.stderr.take().unwrap().read_to_string(&mut error),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(status.code(), Some(1), "{status}: {error}");
    assert!(
        error.contains("editor secret storage disconnected"),
        "{error}"
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match gents::home::lock_store(paths.root(), paths.node_data_dir()) {
                Ok(guard) => {
                    drop(guard);
                    break;
                }
                Err(error) if error.is::<gents::home::StoreLockHeld>() => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("unexpected client lock error: {error}"),
            }
        }
    })
    .await
    .expect("input EOF retained the client store lock");
    assert!(!paths.identity_key_path().exists());
    assert!(!paths.node_data_dir().join("MANIFEST").exists());
}

async fn retained_history(root: PathBuf) -> DesktopPaths {
    let paths = DesktopPaths::from_root(root);
    paths.ensure_root_dirs().await.unwrap();
    {
        let _store = gents::home::lock_store(paths.root(), paths.node_data_dir()).unwrap();
        let record = gents::store_key::StoreEncryption::prepare(
            gents::store_key::StoreKeyCustodyChoice::Keychain,
            paths.store_key_path(),
            paths.node_data_dir(),
        )
        .unwrap();
        std::fs::write(
            paths.store_encryption_path(),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    paths
}
