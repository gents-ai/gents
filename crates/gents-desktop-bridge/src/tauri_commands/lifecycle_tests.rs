use tokio::sync::watch;

use super::core_start::{
    acquire_client_lifecycle, bounded_client_start, classify_client_start_error,
    deliver_client_start, start_client_core_async, ClientStartDelivery, NO_STAGE_COMPLETED,
};
use super::exclusion::shutdown_excluded_client;
use crate::client_runtime::ClientRuntime;

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

    let mut init = tokio::spawn({
        let lifecycle = Arc::clone(&lifecycle);
        async move {
            let _guard = lifecycle.lock().await;
        }
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut init)
            .await
            .is_err(),
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
    let refused = anyhow::Error::new(gents_store::schema::catalog::SchemaError::UnknownLineage {
        collection: "AgentSession".into(),
        versions: "bafy-old".into(),
    })
    .context("ensuring the runtime schema");
    let (error, store) = classify_client_start_error(&refused, node);
    assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
    assert!(
        error
            .message
            .contains("local data at /tmp/desktop/node was made by an older version"),
        "{}",
        error.message
    );
    assert_eq!(
        store.map(|store| store.kind),
        Some(gents_protocol::operations::install_repair::IncompatibleStoreKind::UnknownLineage)
    );

    let refused =
        anyhow::anyhow!("peer directory is locked").context("starting embedded desktop node");
    let (error, store) = classify_client_start_error(&refused, node);
    assert_eq!(error.code, BridgeErrorCode::Unknown);
    assert_eq!(
        error.message,
        "starting embedded desktop node: peer directory is locked"
    );
    assert!(store.is_none());
}

fn test_state() -> (tempfile::TempDir, DesktopAppState) {
    let temp = tempfile::tempdir().expect("tempdir");
    let config = crate::config::BridgeConfig {
        home: crate::config::HomePolicy::FixedRoot(temp.path().join("desktop")),
        bootstrap: crate::config::BootstrapPolicy::PairedRemoteOnly,
        store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
        ..Default::default()
    };
    let policy = crate::state::resolve_policy(&config, None).expect("policy");
    (temp, DesktopAppState::new(policy))
}

#[tokio::test]
async fn cancelled_exclusion_waiter_keeps_lifecycle_guard_until_shutdown_owner_returns() {
    for case in gents_lean_vocab::families::native_claim_lifetime_client_exclusion_scenario::cases::<
        String,
        String,
    >() {
        case.check(
            cancelled_exclusion_waiter_observations()
                .await
                .into_iter()
                .map(|(claim_held, lifecycle_guard_held)| gents_lean_vocab::families::native_claim_lifetime_client_exclusion_scenario::ExclusionView {
                    claim_held,
                    lifecycle_guard_held,
                })
                .collect(),
        );
    }
}

async fn cancelled_exclusion_waiter_observations() -> Vec<(bool, bool)> {
    let mut observed = Vec::new();
    let (_temp, state) = test_state();
    let state = Arc::new(state);
    let paths = state.policy.desktop_paths.clone();
    let core = Arc::new(
        ClientRuntime::start_with_paths_and_options(
            paths.clone(),
            crate::client_test_support::file_journal_options(),
        )
        .await
        .expect("start claimed desktop client"),
    );
    let home = paths.root().to_path_buf();
    let data = paths.node_data_dir().to_path_buf();
    let released = core.store_release();
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
    observed.push((
        gents_store::local::home::lock_store(&home, &data).is_err(),
        Arc::clone(&state.client_lifecycle)
            .try_lock_owned()
            .is_err(),
    ));

    allow_shutdown.notify_one();
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        access_released.notified(),
    )
    .await
    .expect("client shutdown releases the store claim before exclusion ends");
    observed.push((
        gents_store::local::home::lock_store(&home, &data).is_err(),
        Arc::clone(&state.client_lifecycle)
            .try_lock_owned()
            .is_err(),
    ));

    allow_owner_return.notify_one();
    let lifecycle_guard = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Arc::clone(&state.client_lifecycle).lock_owned(),
    )
    .await
    .expect("exclusion guard releases after its shutdown owner returns");
    drop(lifecycle_guard);
    // The owner dropped the client as it returned; its node's engine lets go
    // of the data directory and the store is released from its own thread.
    tokio::time::timeout(std::time::Duration::from_secs(10), released.released())
        .await
        .expect("the store is released once the exclusion's owner let go of the client");
    let lifecycle_guard_held = Arc::clone(&state.client_lifecycle)
        .try_lock_owned()
        .is_err();
    observed.push((
        gents_store::local::home::lock_store(&home, &data).is_err(),
        lifecycle_guard_held,
    ));
    observed
}

#[tokio::test]
async fn a_client_start_cannot_open_the_store_while_a_reset_excludes_it() {
    use std::sync::Arc;
    use std::time::Duration;

    let (_temp, state) = test_state();
    let state = Arc::new(state);

    let (progress, _) = watch::channel(ClientStartProgress::Pending);
    state.bridge.lock().unwrap().start_inflight = Some(progress.clone());
    let mut excluding = {
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            let guard = exclude_client(&state).await.expect("exclude");
            drop(guard);
        })
    };
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut excluding)
            .await
            .is_err(),
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
    let mut starter = {
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
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut starter)
            .await
            .is_err(),
        "no start opens client state during retirement"
    );
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

#[cfg(unix)]
#[tokio::test]
async fn an_insecure_old_client_key_fails_start_as_an_incompatible_local_store() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().expect("tempdir");
    let paths = gents_store::client_journal::paths::DesktopPaths::from_root(temp.path());
    gents_store::identity::load_or_create_file_identity(paths.identity_key_path()).unwrap();
    std::fs::set_permissions(
        paths.identity_key_path(),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();

    let lifecycle = Arc::new(tokio::sync::Mutex::new(())).lock_owned().await;
    let Err((error, store)) = start_client_core_async(
        paths.clone(),
        gents_store::local::store_key::StoreKeyCustodyChoice::File,
        lifecycle,
    )
    .await
    else {
        panic!("an insecure key must not start the client");
    };
    assert_eq!(error.code, BridgeErrorCode::IncompatibleLocalStore);
    assert!(error.message.contains("chmod 600"), "{}", error.message);
    let store = store.expect("typed refusal");
    assert_eq!(
        store.kind,
        gents_protocol::operations::install_repair::IncompatibleStoreKind::InsecureKey
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
    assert!(!paths.runtime_directory_path().exists());
}

#[tokio::test]
async fn a_remote_only_start_sets_aside_another_versions_client_store() {
    use crate::config::{BootstrapPolicy, BridgeConfig, HomePolicy};
    use tauri::Manager as _;
    let temp = tempfile::tempdir().unwrap();
    let policy = crate::state::resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("client")),
            bootstrap: BootstrapPolicy::PairedRemoteOnly,
            store_key_custody: gents_store::local::store_key::StoreKeyCustodyChoice::File,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    let paths = policy.desktop_paths.clone();
    gents::client_test_support::write_other_version_client_store(&paths, true, false).await;
    let identity = std::fs::read(paths.identity_key_path()).unwrap();
    let app = tauri::test::mock_builder()
        .manage(DesktopAppState::new(policy))
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let snapshot = desktop_client_start(app.handle().clone(), app.state::<DesktopAppState>())
        .await
        .expect("the client starts on a fresh store");
    let note = snapshot
        .repair_note
        .expect("the start names what it set aside");
    assert!(note.contains("another Gents version"), "{note}");
    assert!(snapshot.client.is_some());
    assert_eq!(std::fs::read(paths.identity_key_path()).unwrap(), identity);
    let backup = std::fs::read_dir(temp.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.join("node/MANIFEST").is_file())
        .expect("the backup holds the old store");
    assert!(note.contains(&backup.display().to_string()), "{note}");
    drop(
        exclusion::exclude_client(&app.state::<DesktopAppState>())
            .await
            .unwrap(),
    );
}

/// Selecting a node reads it at once unless its observer is connecting it
/// (Lean `RuntimeConnectionObservation.selectionReadsNode`), for every status
/// and for a node not yet observed.
#[test]
fn selecting_a_node_reads_it_unless_its_observer_is_connecting() {
    use gents_observe::client_projection::observe::RuntimeConnectionStatus;

    #[derive(Clone, Copy, Debug, serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum Status {
        Connecting,
        Connected,
        Disconnected,
        Refused,
        NoEndpoint,
    }

    let cases = gents_lean_vocab::families::runtime_connection_observation_selection_reads::cases::<
        Status,
    >();
    assert!(!cases.is_empty(), "Lean emitted no selection cases");
    for case in cases {
        let status = match case.input.status {
            Status::Connecting => RuntimeConnectionStatus::Connecting,
            Status::Connected => RuntimeConnectionStatus::Connected,
            Status::Disconnected => RuntimeConnectionStatus::Disconnected,
            Status::Refused => RuntimeConnectionStatus::Refused,
            Status::NoEndpoint => RuntimeConnectionStatus::NoEndpoint,
        };
        let observed = case.input.observed.then_some(status);
        case.check(selection_reads_node(observed));
    }
}
