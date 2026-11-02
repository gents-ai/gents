use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use tauri::Manager;
use tokio_util::codec::LengthDelimitedCodec;

use super::*;
use tokio::sync::watch;

use super::single_flight::run_detached_client_start;
use crate::config::{BootstrapPolicy, BridgeConfig, HomePolicy};
use crate::host::{local, protocol};
use crate::state::resolve_policy;
use crate::state::ClientStartProgress;

#[tokio::test]
async fn an_existing_listener_reservation_prevents_desktop_from_opening_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("client")),
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            client_host: true,
            store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let paths = policy.desktop_paths.clone();
    let listener = local::LocalListener::bind(&paths).await.unwrap();
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(policy))
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let (progress, receiver) = watch::channel(ClientStartProgress::Pending);
    run_detached_client_start(app.handle().clone(), paths.clone(), progress).await;
    let error = wait_for_client_start_progress(receiver).await.unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
    assert!(current_core(&app.state::<DesktopAppState>()).is_none());
    assert!(!paths.principal_metadata_path().exists());
    let claim = gents::store::local::home::lock_store(paths.root(), paths.node_data_dir()).unwrap();
    drop(claim);
    drop(listener);
    let (progress, receiver) = watch::channel(ClientStartProgress::Pending);
    run_detached_client_start(app.handle().clone(), paths, progress).await;
    wait_for_client_start_progress(receiver).await.unwrap();
    drop(
        exclude_client(&app.state::<DesktopAppState>())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn desktop_start_publishes_its_client_and_exclusion_closes_attachments_before_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("client")),
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            client_host: true,
            store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let paths = policy.desktop_paths.clone();
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(policy))
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let (progress, receiver) = watch::channel(ClientStartProgress::Pending);
    run_detached_client_start(app.handle().clone(), paths.clone(), progress).await;
    wait_for_client_start_progress(receiver).await.unwrap();
    let state = app.state::<DesktopAppState>();
    let core = current_core(&state).unwrap();
    let peer = core.local_peer_id().to_owned();
    core.set_selected_node_did(Some("did:scope:desktop".into()));
    let stream = local::connect(&paths).await.unwrap();
    let mut client = LengthDelimitedCodec::builder().new_framed(stream);
    client
        .send(Bytes::from(
            serde_json::to_vec(&serde_json::json!({
                "kind": "hello",
                "version": protocol::PROTOCOL_VERSION,
            }))
            .unwrap(),
        ))
        .await
        .unwrap();
    let hello: serde_json::Value =
        serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
    assert_eq!(core.selected_node_did(), None);
    client
        .send(Bytes::from_static(
            br#"{"kind":"call","id":1,"command":{"name":"desktop_client_snapshot","args":{}}}"#,
        ))
        .await
        .unwrap();
    loop {
        let reply: serde_json::Value =
            serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
        if reply["kind"] == "response" {
            assert!(reply["error"].is_null(), "{reply}");
            assert_eq!(reply["result"]["client"]["localPeerId"], peer);
            assert_eq!(reply["result"]["bootstrap"]["desktopHome"], "");
            break;
        }
    }
    let exclusion = exclude_client(&state).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(frame) = client.next().await {
            frame.unwrap();
        }
    })
    .await
    .expect("exclusion closes the native attachment");
    assert!(local::connect(&paths).await.is_err());
    assert_eq!(
        core.selected_node_did().as_deref(),
        Some("did:scope:desktop")
    );
    assert!(current_core(&state).is_none());
    assert!(Arc::clone(&state.client_lifecycle)
        .try_lock_owned()
        .is_err());
    let claim = gents::store::local::home::lock_store(paths.root(), paths.node_data_dir()).unwrap();
    drop(claim);
    drop(exclusion);
}
