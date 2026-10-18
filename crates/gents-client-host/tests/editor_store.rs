#![cfg(target_os = "macos")]

use std::process::Stdio;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::local;
use gents_desktop_core::client::DesktopPaths;
use serde_json::{json, Value};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

#[tokio::test]
async fn an_editor_cache_miss_stops_before_identity_or_store_startup() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("editor client"));
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
    assert_eq!(error["requiresUnlock"], true);
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
    assert_eq!(next_error["requiresUnlock"], true);
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
