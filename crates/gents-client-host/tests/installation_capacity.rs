#![cfg(any(unix, windows))]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::{local, protocol, ClientHost};
use gents_desktop_bridge::{BridgeError, BridgeErrorCode};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_full_listener_reports_a_retryable_initialization_failure_without_retiring_its_clients() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("busy history"));
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let peer = core.local_peer_id().to_owned();
    let listener = local::LocalListener::bind(&paths).await.unwrap();
    let host = Arc::new(ClientHost::for_installation(
        core.clone(),
        "busy-build".into(),
    ));
    let stop = CancellationToken::new();
    let serving = tokio::spawn(listener.serve(host, stop.clone()));
    let mut clients = Vec::new();
    for _ in 0..local::MAX_ATTACHMENTS {
        let mut client = LengthDelimitedCodec::builder()
            .max_frame_length(protocol::MAX_FRAME_BYTES)
            .new_framed(local::connect(&paths).await.unwrap());
        client
            .send(Bytes::from(
                serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                    .unwrap(),
            ))
            .await
            .unwrap();
        let hello: Value = serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
        assert_eq!(hello["installationBuild"], "busy-build");
        clients.push(client);
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = FramedRead::new(child.stdout.take().unwrap(), LengthDelimitedCodec::new());
    let frame = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let message: Value = serde_json::from_slice(&frame).unwrap();
    assert_eq!(message["kind"], "error");
    let error: BridgeError = serde_json::from_value(message["error"].clone()).unwrap();
    assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
    assert!(error.retryable);
    assert!(!tokio::time::timeout(Duration::from_secs(10), child.wait())
        .await
        .unwrap()
        .unwrap()
        .success());
    assert!(!stop.is_cancelled());
    clients[0]
        .send(Bytes::from_static(
            br#"{"kind":"call","id":1,"command":{"name":"desktop_client_snapshot","args":{}}}"#,
        ))
        .await
        .unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = clients[0].next().await.unwrap().unwrap();
            let message: Value = serde_json::from_slice(&frame).unwrap();
            if message["kind"] == "response" && message["id"] == 1 {
                break message;
            }
        }
    })
    .await
    .unwrap();
    assert!(response["error"].is_null());
    assert_eq!(response["result"]["client"]["localPeerId"], peer);
    drop(clients);
    stop.cancel();
    serving.await.unwrap().unwrap();
    core.shutdown().await.unwrap();
}
