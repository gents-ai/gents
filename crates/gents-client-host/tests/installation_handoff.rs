#![cfg(any(unix, windows))]

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents_desktop_bridge::host::{local, protocol};
use gents_desktop_bridge::{BridgeError, BridgeErrorCode};
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths};
use serde_json::{json, Value};
use tokio::process::Command;
use tokio_util::codec::{Framed, FramedRead, LengthDelimitedCodec};

mod support;
use support::Attachment;

type Connection = Framed<local::Connection, LengthDelimitedCodec>;

async fn hello(paths: &DesktopPaths) -> (Connection, Value) {
    let stream = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(stream) = local::connect(paths).await {
                break stream;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("installation did not open its endpoint");
    let mut connection = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(stream);
    connection
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind":"hello", "version":protocol::PROTOCOL_VERSION}))
                .unwrap(),
        ))
        .await
        .unwrap();
    let frame = tokio::time::timeout(Duration::from_secs(10), connection.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let hello = serde_json::from_slice(&frame).unwrap();
    (connection, hello)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires GENTS_PREVIOUS_CLIENT_HOST naming a retained pre-retirement helper"]
async fn a_host_without_retirement_control_releases_history_through_its_idle_lifetime() {
    let previous_binary = std::env::var_os("GENTS_PREVIOUS_CLIENT_HOST")
        .expect("provide the retained predecessor helper to qualify its real lifetime");
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("retained history"));
    let seed = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let peer = seed.local_peer_id().to_owned();
    let client_did = seed.node_identity().did().to_owned();
    let did = gents::graphql::escape_graphql_string(&client_did);
    gents::config_client::ConfigAccess::write_local(
        &seed.node_arc(),
        "installation_handoff.seed",
        &format!(
            r#"mutation {{ create_AgentSession(input: {{session_id: "retained-session",
            node_did: "{did}", requester_did: "{did}", agent_id: "agent",
            created_at: "2026-10-25T00:00:00Z"}}) {{ _docID }} }}"#
        ),
    )
    .await
    .unwrap();
    seed.shutdown().await.unwrap();
    drop(seed);
    let original_record = std::fs::read(paths.store_encryption_path()).unwrap();
    let log = std::fs::File::create(directory.path().join("previous.log")).unwrap();
    let mut previous = Command::new(previous_binary)
        .args(["--serve", "--home"])
        .arg(paths.root())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (connection, old_hello) = hello(&paths).await;
    assert_eq!(old_hello["version"], protocol::PROTOCOL_VERSION);
    assert!(old_hello.get("installationControlVersion").is_none());
    drop(connection);

    let mut attachment = Command::new(env!("CARGO_BIN_EXE_gents-client-host"))
        .args(["--stdio", "--editor-store", "--home"])
        .arg(paths.root())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut output = FramedRead::new(
        attachment.stdout.take().unwrap(),
        LengthDelimitedCodec::new(),
    );
    let frame = tokio::time::timeout(Duration::from_secs(30), output.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let message: Value = serde_json::from_slice(&frame).unwrap();
    assert_eq!(message["kind"], "error");
    let error: BridgeError = serde_json::from_value(message["error"].clone()).unwrap();
    assert_eq!(error.code, BridgeErrorCode::Unsupported);
    assert!(!error.retryable);
    assert!(
        !tokio::time::timeout(Duration::from_secs(10), attachment.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    assert_eq!(
        std::fs::read(paths.store_encryption_path()).unwrap(),
        original_record
    );
    let status = tokio::time::timeout(Duration::from_secs(210), previous.wait())
        .await
        .expect("the predecessor did not release history after its idle lifetime")
        .unwrap();
    assert!(status.success());

    let mut updated = Attachment::start(&paths).await;
    let snapshot = updated.snapshot(1).await;
    assert_eq!(snapshot["client"]["localPeerId"], peer);
    assert_eq!(snapshot["client"]["clientDid"], client_did);
    updated.close().await;
    let (mut connection, current) = hello(&paths).await;
    assert_eq!(
        current["installationControlVersion"],
        protocol::INSTALLATION_CONTROL_VERSION
    );
    connection
        .send(Bytes::from(
            serde_json::to_vec(
                &json!({"kind":"retire", "id":2, "expectedBuild":current["installationBuild"]}),
            )
            .unwrap(),
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        let mut acknowledged = false;
        while let Some(frame) = connection.next().await {
            let message: Value = serde_json::from_slice(&frame.unwrap()).unwrap();
            if message["kind"] == "response" && message["id"] == 2 {
                assert!(message["error"].is_null());
                acknowledged = true;
            }
        }
        assert!(acknowledged);
        loop {
            match gents::home::lock_store(paths.root(), paths.node_data_dir()) {
                Ok(lock) => {
                    drop(lock);
                    break;
                }
                Err(error) if error.lock_held().is_some() => {
                    tokio::time::sleep(Duration::from_millis(20)).await
                }
                Err(error) => panic!("{error:#}"),
            }
        }
    })
    .await
    .unwrap();
    let reopened = Arc::new(
        ClientCore::start_with_paths_and_options(paths.clone(), ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    assert_eq!(reopened.node_identity().did(), client_did);
    let data = gents::config_client::ConfigAccess::local(reopened.node_arc())
        .execute("{ AgentSession { session_id } }")
        .await
        .unwrap();
    assert_eq!(
        data["data"]["AgentSession"][0]["session_id"],
        "retained-session"
    );
    assert_eq!(
        std::fs::read(paths.store_encryption_path()).unwrap(),
        original_record
    );
    reopened.shutdown().await.unwrap();
}
