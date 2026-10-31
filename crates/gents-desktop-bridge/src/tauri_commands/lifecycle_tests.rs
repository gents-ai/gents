use tokio::sync::watch;

use super::core_start::{
    acquire_client_lifecycle, bounded_client_start, classify_client_start_error,
    deliver_client_start, start_client_core_async, ClientStartDelivery, NO_STAGE_COMPLETED,
};
use super::exclusion::shutdown_excluded_client;
use gents_desktop_core::client::ClientCore;

use super::*;
use crate::state::ClientStartProgress;

#[tokio::test]
async fn a_stalled_client_start_keeps_the_store_locked_until_its_core_closes() {
    let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
    let (delivery_tx, delivery_rx) = tokio::sync::oneshot::channel::<ClientStartDelivery<u8>>();
    let (stage_tx, stage_rx) = watch::channel(NO_STAGE_COMPLETED);
    let starter_guard = Arc::clone(&lifecycle).lock_owned().await;
    stage_tx.send_replace("embedded_node");

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        bounded_client_start(
            delivery_rx,
            stage_rx,
            std::time::Duration::from_millis(20),
            std::path::Path::new("/tmp/node"),
        ),
    )
    .await
    .expect("a stalled start is bounded")
    .expect_err("a stalled start fails")
    .0;
    assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
    assert!(error.message.contains("embedded_node"), "{}", error.message);

    let init = tokio::spawn({
        let lifecycle = Arc::clone(&lifecycle);
        async move {
            let _guard = lifecycle.lock().await;
        }
    });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        !init.is_finished(),
        "the store is still owned by the late start"
    );

    let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    deliver_client_start(delivery_tx, Ok(7), starter_guard, {
        let closed = Arc::clone(&closed);
        move |_late: u8| async move {
            assert!(
                lifecycle.try_lock().is_err(),
                "the lock is released only after the late core closes"
            );
            closed.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok::<(), std::convert::Infallible>(())
        }
    })
    .await;
    assert!(closed.load(std::sync::atomic::Ordering::SeqCst));
    tokio::time::timeout(std::time::Duration::from_secs(5), init)
        .await
        .expect("init proceeds once the late core is closed")
        .unwrap();
}

#[tokio::test]
async fn a_retry_reports_a_previous_start_that_is_still_closing() {
    let lifecycle = Arc::new(tokio::sync::Mutex::new(()));
    let held = Arc::clone(&lifecycle).lock_owned().await;
    let error =
        acquire_client_lifecycle(Arc::clone(&lifecycle), std::time::Duration::from_millis(20))
            .await
            .expect_err("the store is still owned by the stalled start");
    assert_eq!(error.code, BridgeErrorCode::ClientStartFailed);
    assert!(error.message.contains("still closing"), "{}", error.message);
    drop(held);
    acquire_client_lifecycle(lifecycle, std::time::Duration::from_millis(20))
        .await
        .expect("a retry proceeds once the late core is closed");
}

#[test]
fn an_older_client_store_fails_start_as_an_incompatible_local_store() {
    let node = std::path::Path::new("/tmp/desktop/node");
    let refused = anyhow::Error::new(
        gents::store::schema::migration::MigrationError::UnknownLineage {
            collection: "AgentSession".into(),
            versions: "bafy-old".into(),
        },
    )
    .context("ensure_migrations");
    let (error, store) = classify_client_start_error(&refused, node);
    assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
    assert!(
        error.message.contains("/tmp/desktop/node"),
        "{}",
        error.message
    );
    assert_eq!(
        store.map(|store| store.kind),
        Some(gents::store::local::storage_backend::IncompatibleStoreKind::UnknownLineage)
    );

    let (error, store) =
        classify_client_start_error(&anyhow::anyhow!("peer directory is locked"), node);
    assert_eq!(error.code, BridgeErrorCode::Unknown);
    assert!(store.is_none());
}

fn test_state() -> (tempfile::TempDir, DesktopAppState) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = crate::config::BridgeConfig {
        home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
        store_key_custody: gents::store::local::store_key::StoreKeyCustodyChoice::File,
        ..Default::default()
    };
    let policy = crate::state::resolve_policy(&config, None).expect("policy");
    (temp, DesktopAppState::new(policy))
}

#[tokio::test]
async fn cancelled_exclusion_waiter_keeps_lifecycle_guard_until_shutdown_owner_returns() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../gents/tests/fixtures/native_claim_lifetime.json"
    ))
    .expect("generated native claim lifetime fixture");
    let case = fixture
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "client_shutdown_cancel_retains_claim_until_return")
        .expect("Lean client shutdown cancellation case");
    let events = case["events"].as_array().unwrap();
    let expected_events = [
        "native_started",
        "caller_cancelled",
        "native_returned",
        "access_released",
        "lifecycle_guard_released",
        "gate_release_requested",
    ]
    .map(|event| serde_json::Value::String(event.to_owned()));
    assert_eq!(events, expected_events.as_slice());
    let observation_for = |event: &str| {
        let index = events
            .iter()
            .position(|candidate| candidate == event)
            .expect("event in generated trace");
        &case["observations"][index]
    };
    let lifecycle_guard_held_for = |event: &str| {
        observation_for(event)["lifecycle_guard_held"]
            .as_bool()
            .expect("lifecycle guard observation in generated trace")
    };
    let claim_held_for = |event: &str| {
        observation_for(event)["claim_held"]
            .as_bool()
            .expect("claim observation in generated trace")
    };

    let (_temp, state) = test_state();
    let state = Arc::new(state);
    let paths = state.policy.desktop_paths.clone();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(
            paths.clone(),
            gents_desktop_core::client::ClientCoreOptions::local_only(),
        )
        .await
        .expect("start claimed desktop client"),
    );
    let home = paths.root().to_path_buf();
    let data = paths.node_data_dir().to_path_buf();
    let lifecycle_guard = Arc::clone(&state.client_lifecycle).lock_owned().await;
    let started = Arc::new(tokio::sync::Notify::new());
    let allow_shutdown = Arc::new(tokio::sync::Notify::new());
    let access_released = Arc::new(tokio::sync::Notify::new());
    let allow_owner_return = Arc::new(tokio::sync::Notify::new());
    let shutdown_started = Arc::clone(&started);
    let shutdown_allowed = Arc::clone(&allow_shutdown);
    let access_released_signal = Arc::clone(&access_released);
    let owner_return_allowed = Arc::clone(&allow_owner_return);
    let waiter = tokio::spawn(async move {
        shutdown_excluded_client(lifecycle_guard, async move {
            shutdown_started.notify_one();
            shutdown_allowed.notified().await;
            core.shutdown()
                .await
                .map_err(|error| BridgeError::untyped(error.to_string()))?;
            access_released_signal.notify_one();
            owner_return_allowed.notified().await;
            Ok(())
        })
        .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(10), started.notified())
        .await
        .expect("owned exclusion shutdown task started");
    waiter.abort();
    assert!(waiter.await.unwrap_err().is_cancelled());
    assert_eq!(
        gents::store::local::home::lock_store(&home, &data).is_err(),
        claim_held_for("caller_cancelled")
    );
    assert_eq!(
        Arc::clone(&state.client_lifecycle)
            .try_lock_owned()
            .is_err(),
        lifecycle_guard_held_for("caller_cancelled")
    );

    allow_shutdown.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        access_released.notified(),
    )
    .await
    .expect("client shutdown releases the store claim before exclusion ends");
    assert_eq!(
        gents::store::local::home::lock_store(&home, &data).is_err(),
        claim_held_for("access_released")
    );
    assert_eq!(
        Arc::clone(&state.client_lifecycle)
            .try_lock_owned()
            .is_err(),
        lifecycle_guard_held_for("access_released")
    );

    allow_owner_return.notify_one();
    let lifecycle_guard = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Arc::clone(&state.client_lifecycle).lock_owned(),
    )
    .await
    .expect("exclusion guard releases after its shutdown owner returns");
    drop(lifecycle_guard);
    assert_eq!(
        Arc::clone(&state.client_lifecycle)
            .try_lock_owned()
            .is_err(),
        lifecycle_guard_held_for("lifecycle_guard_released")
    );
    assert_eq!(
        gents::store::local::home::lock_store(&home, &data).is_err(),
        claim_held_for("lifecycle_guard_released"),
        "the native store claim is released before the lifecycle guard"
    );
}

#[tokio::test]
async fn a_client_start_cannot_open_the_store_while_a_reset_excludes_it() {
    use std::sync::Arc;
    use std::time::Duration;

    let (_temp, state) = test_state();
    let state = Arc::new(state);

    let (progress, _) = watch::channel(ClientStartProgress::Pending);
    state.bridge.lock().unwrap().start_inflight = Some(progress.clone());
    let excluding = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let guard = exclude_client(&state).await.expect("exclude");
            drop(guard);
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !excluding.is_finished(),
        "an in-flight start is drained first"
    );
    state.bridge.lock().unwrap().start_inflight = None;
    let _ = progress.send(ClientStartProgress::Ready);
    tokio::time::timeout(Duration::from_secs(5), excluding)
        .await
        .expect("exclusion follows the drained start")
        .unwrap();

    let guard = exclude_client(&state).await.expect("exclude");
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let opened = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let starter = {
        let state = Arc::clone(&state);
        let barrier = Arc::clone(&barrier);
        let opened = Arc::clone(&opened);
        tokio::spawn(async move {
            barrier.wait().await;
            let _lifecycle = state.client_lifecycle.lock().await;
            opened.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    };
    barrier.wait().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !opened.load(std::sync::atomic::Ordering::SeqCst),
        "no start opens client state during retirement"
    );
    drop(guard);
    tokio::time::timeout(Duration::from_secs(5), starter)
        .await
        .expect("the start proceeds after the reset")
        .unwrap();
    assert!(opened.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn a_plaintext_client_upgrade_returns_a_started_core_with_lifecycle_exclusion() {
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = gents_desktop_core::client::DesktopPaths::from_root(temp.path());
    let legacy = gents::defra_node::EmbeddedNode::builder()
        .data_path(paths.node_data_dir())
        .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
        .build()
        .await
        .expect("legacy client store");
    legacy.shutdown().await;
    drop(legacy);

    let lifecycle_owner = Arc::new(tokio::sync::Mutex::new(()));
    let lifecycle = Arc::clone(&lifecycle_owner).lock_owned().await;
    let (core, lifecycle) = start_client_core_async(
        paths.clone(),
        gents::store::local::store_key::StoreKeyCustodyChoice::File,
        lifecycle,
    )
    .await
    .expect("plaintext upgrade must complete through the native startup boundary");
    assert!(
        lifecycle_owner.try_lock().is_err(),
        "successful startup transfers lifecycle exclusion with the core"
    );
    assert!(paths.store_encryption_path().is_file());
    assert!(paths.store_key_path().is_file());
    assert!(
        gents::store::local::store_key::upgrade::pending_record(paths.node_data_dir())
            .unwrap()
            .is_none()
    );
    for entry in [paths.store_encryption_path(), paths.store_key_path()] {
        assert!(
            paths
                .client_state_entries()
                .iter()
                .any(|path| path == entry),
            "the reset retires {}",
            entry.display()
        );
    }
    let core = Arc::new(core);
    core.shutdown().await.expect("shutdown upgraded client");
    drop(core);
    drop(lifecycle);
    assert!(lifecycle_owner.try_lock().is_ok());
}

#[cfg(unix)]
#[tokio::test]
async fn an_insecure_old_client_key_fails_start_as_an_incompatible_local_store() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = gents_desktop_core::client::DesktopPaths::from_root(temp.path());
    gents::store::identity::load_or_create_file_identity(paths.identity_key_path()).unwrap();
    std::fs::set_permissions(
        paths.identity_key_path(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let lifecycle = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;
    let Err((error, store)) = start_client_core_async(
        paths.clone(),
        gents::store::local::store_key::StoreKeyCustodyChoice::File,
        lifecycle,
    )
    .await
    else {
        panic!("an insecure key must not start the client");
    };
    assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
    assert!(
        error.message.contains("unsafe file permissions"),
        "{}",
        error.message
    );
    let store = store.expect("typed refusal");
    assert_eq!(
        store.kind,
        gents::store::local::storage_backend::IncompatibleStoreKind::InsecureKey
    );
    assert_eq!(store.data_path, paths.identity_key_path());
    assert_eq!(
        std::fs::metadata(paths.identity_key_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
    assert!(!paths.peer_directory_path().exists());
}
