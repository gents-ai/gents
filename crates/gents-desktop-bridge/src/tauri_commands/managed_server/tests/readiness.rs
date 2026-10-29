use super::*;

#[tokio::test]
async fn a_slow_booting_runtime_is_waited_on_until_it_publishes_readiness() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let probes = AtomicUsize::new(0);
    let ready = await_runtime_readiness(
        Duration::from_secs(5),
        Duration::from_millis(5),
        || {
            let attempt = probes.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(if attempt < 5 {
                    PortReadiness::NotListening
                } else {
                    PortReadiness::Ready(ready_status("did:key:slow"))
                })
            }
        },
        || async { Ok(NativeProgress::Loaded) },
    )
    .await
    .expect("a loaded job that is still booting is waited on");

    let Readiness::Ready(ready) = ready else {
        panic!("expected readiness");
    };
    assert_eq!(ready.node_did.as_deref(), Some("did:key:slow"));
    assert_eq!(probes.load(Ordering::SeqCst), 6);
}

#[test]
fn this_homes_runtime_is_ready_only_once_it_reports_ready() {
    let did = "did:key:managed";
    let payload = |lifecycle: serde_json::Value| {
        serde_json::json!({
            "node_did": did,
            "node_name": "Managed",
            "graphql": "http://127.0.0.1:9191/api/v0/graphql",
            gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: lifecycle,
        })
    };
    for booting in [serde_json::Value::Null, serde_json::json!("starting")] {
        assert!(matches!(
            classify_port_payload(9191, Some(did), payload(booting)),
            PortReadiness::Booting
        ));
    }
    let PortReadiness::Ready(status) =
        classify_port_payload(9191, Some(did), payload(serde_json::json!("ready")))
    else {
        panic!("a runtime that reports ready is ready");
    };
    assert_eq!(status.node_did.as_deref(), Some(did));
    assert!(matches!(
        classify_port_payload(
            9191,
            Some("did:key:other"),
            payload(serde_json::json!("ready"))
        ),
        PortReadiness::Foreign(ForeignRuntime { port: 9191, .. })
    ));
}

#[tokio::test]
async fn a_booting_runtime_is_waited_on_until_it_reports_ready() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let probes = AtomicUsize::new(0);
    let ready = await_runtime_readiness(
        Duration::from_secs(5),
        Duration::from_millis(5),
        || {
            let attempt = probes.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(if attempt < 3 {
                    PortReadiness::Booting
                } else {
                    PortReadiness::Ready(ready_status("did:key:booted"))
                })
            }
        },
        || async { Ok(NativeProgress::Loaded) },
    )
    .await
    .expect("a bound runtime that is still starting is waited on");
    assert!(matches!(ready, Readiness::Ready(_)));
    assert_eq!(probes.load(Ordering::SeqCst), 4);
}

#[tokio::test]
async fn a_runtime_that_predates_readiness_fails_the_wait_at_once() {
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        await_runtime_readiness(
            Duration::from_secs(300),
            Duration::from_millis(5),
            || async {
                Ok(PortReadiness::Outdated {
                    version: Some("0.18.2".to_string()),
                })
            },
            || async { Ok(NativeProgress::Loaded) },
        ),
    )
    .await
    .expect("an outdated runtime is not waited on")
    .unwrap_err();
    assert!(
        error.to_string().contains("(v0.18.2) predates this app"),
        "{error}"
    );
}

#[test]
fn an_older_runtime_without_the_lifecycle_field_is_outdated() {
    let did = "did:key:managed";
    let observed = classify_port_payload(
        9191,
        Some(did),
        serde_json::json!({ "node_did": did, "version": "0.18.2", "status": "ok" }),
    );
    let PortReadiness::Outdated { version } = observed else {
        panic!("an older runtime is outdated, not booting");
    };
    assert_eq!(version.as_deref(), Some("0.18.2"));
}

#[tokio::test]
async fn a_booting_runtime_past_the_bound_is_left_running() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let probes = AtomicUsize::new(0);
    let error = await_runtime_readiness(
        Duration::from_millis(60),
        Duration::from_millis(5),
        || {
            let attempt = probes.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(if attempt < 4 {
                    PortReadiness::NotListening
                } else {
                    PortReadiness::Booting
                })
            }
        },
        || async { Ok(NativeProgress::Loaded) },
    )
    .await
    .expect_err("the wait is bounded");
    assert!(error.is::<RuntimeStillBooting>(), "{error}");

    let started = std::time::Instant::now();
    await_runtime_readiness(
        Duration::from_millis(200),
        Duration::from_millis(5),
        || {
            let elapsed = started.elapsed();
            async move {
                Ok(if elapsed < Duration::from_millis(150) {
                    PortReadiness::NotListening
                } else if elapsed < Duration::from_millis(300) {
                    PortReadiness::Booting
                } else {
                    PortReadiness::Ready(ready_status("did:key:migrated"))
                })
            }
        },
        || async { Ok(NativeProgress::Loaded) },
    )
    .await
    .expect("binding restarts the bound");
}

#[tokio::test]
async fn cancelling_the_approval_wait_returns_without_waiting_out_its_window() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let cancel = tokio_util::sync::CancellationToken::new();
    let approvals = AtomicU32::new(0);
    let wait = await_managed_pairing_approval(
        &cancel,
        Duration::from_secs(600),
        Duration::from_millis(5),
        || async { false },
        || async {
            approvals.fetch_add(1, Ordering::SeqCst);
            Ok(PairingApproval::NotVisibleYet)
        },
        || async { Ok(()) },
    );
    let cancelled = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(wait, cancelled)
    })
    .await
    .expect("a drain is not held for the approval window");
    assert_eq!(result.unwrap_err(), PairingFailure::Cancelled);
    assert!(
        approvals.load(Ordering::SeqCst) > 1,
        "approval was retried while waiting"
    );
}

#[tokio::test]
async fn an_approval_is_committed_once_then_waits_for_chat_readiness() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let cancel = tokio_util::sync::CancellationToken::new();
    let (checks, approvals, repairs) = (AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0));
    await_managed_pairing_approval(
        &cancel,
        Duration::from_secs(5),
        Duration::from_millis(1),
        || async { checks.fetch_add(1, Ordering::SeqCst) >= 3 },
        || async {
            approvals.fetch_add(1, Ordering::SeqCst);
            Ok(PairingApproval::Committed)
        },
        || async {
            repairs.fetch_add(1, Ordering::SeqCst);
            Ok(())
        },
    )
    .await
    .expect("paired");
    assert_eq!(approvals.load(Ordering::SeqCst), 1);
    assert_eq!(repairs.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn readiness_wait_fails_as_soon_as_the_service_stops() {
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async { Ok(NativeProgress::Stopped) },
    )
    .await
    .expect_err("an exited service must not be waited on");
    assert!(error.to_string().contains("stopped"), "{error}");
}

#[tokio::test]
async fn a_crash_looping_service_fails_with_its_exit_reason() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let observations = AtomicUsize::new(0);
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || {
            let seen = observations.fetch_add(1, Ordering::SeqCst) as u64;
            async move { Ok(NativeProgress::Exited(service_exit(1 + seen / 4))) }
        },
    )
    .await
    .expect_err("a crash loop must not be waited out");
    assert!(error.to_string().contains("exited with code 78"), "{error}");
    assert!(
        error.to_string().contains("restarted 2 times in a row"),
        "{error}"
    );
    assert_eq!(observations.load(Ordering::SeqCst), 9);
}

fn refused_store_exit(restarts: u64) -> gents_server::native_service::ServiceExit {
    gents_server::native_service::ServiceExit {
        reason: format!(
            "exited with code {}",
            gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE
        ),
        restarts,
        clean: false,
        code: Some(gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE),
    }
}

#[tokio::test]
async fn a_refused_store_fails_the_start_typed_on_its_first_exit() {
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async { Ok(NativeProgress::Exited(refused_store_exit(0))) },
    )
    .await
    .expect_err("a refused store is final");
    let typed = error
        .downcast_ref::<BridgeError>()
        .expect("typed bridge error");
    assert_eq!(typed.code, BridgeErrorCode::IncompatibleLocalStore);
}

#[tokio::test]
async fn a_refused_store_fails_typed_even_while_status_reports_booting() {
    for code in [
        gents_server::native_service::INCOMPATIBLE_STORE_EXIT_CODE,
        gents_server::native_service::INSECURE_KEY_EXIT_CODE,
        gents_server::native_service::FOREIGN_STORE_EXIT_CODE,
    ] {
        let error = await_runtime_readiness(
            Duration::from_secs(60),
            Duration::from_millis(5),
            || async { Ok(PortReadiness::Booting) },
            || async move {
                Ok(NativeProgress::Exited(
                    gents_server::native_service::ServiceExit {
                        reason: format!("exited with code {code}"),
                        restarts: 0,
                        clean: false,
                        code: Some(code),
                    },
                ))
            },
        )
        .await
        .expect_err("a refused store is final");
        assert_eq!(
            error.downcast_ref::<BridgeError>().map(|error| error.code),
            Some(BridgeErrorCode::IncompatibleLocalStore),
            "exit {code}"
        );
        assert!(refused_kind(&error).is_some(), "exit {code}");
    }
}

#[test]
fn a_refused_store_exit_reports_an_incompatible_home_not_a_crash_loop() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        detail: None,
        background_item: Default::default(),
        failed: false,
    };
    let managed = ManagedServerRuntimeState {
        incompatible_store: Some(lineage_refusal(PathBuf::from("/tmp/.gents/data"))),
        ..Default::default()
    };
    let status = status_from(
        &managed,
        None,
        Some(&native),
        Some(&refused_store_exit(0)),
        None,
    );
    assert_eq!(status.state, ManagedServerState::Failed);
    assert_eq!(
        status.error_code,
        Some(BridgeErrorCode::IncompatibleLocalStore)
    );
    assert!(status
        .error
        .as_deref()
        .is_some_and(|error| error.contains("/tmp/.gents/data")));

    let starting = ManagedServerRuntimeState {
        starting: true,
        ..Default::default()
    };
    let status = status_from(
        &starting,
        None,
        Some(&native),
        Some(&refused_store_exit(0)),
        None,
    );
    assert_eq!(status.state, ManagedServerState::Starting);
    assert_eq!(
        status.error_code, None,
        "a start in progress reports its own failure"
    );

    let exit = service_exit(3);
    let crashed = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        Some(&native),
        Some(&exit),
        Some((&exit, 3)),
    );
    assert_eq!(crashed.state, ManagedServerState::Failed);
    assert_eq!(crashed.error_code, None);
}

#[tokio::test]
async fn a_single_exit_between_respawns_is_not_a_crash_loop() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let observations = AtomicUsize::new(0);
    let ready = await_runtime_readiness(
        Duration::from_secs(5),
        Duration::from_millis(1),
        || {
            let seen = observations.load(Ordering::SeqCst);
            async move {
                Ok(if seen >= 30 {
                    PortReadiness::Ready(ready_status("did:key:respawned"))
                } else {
                    PortReadiness::NotListening
                })
            }
        },
        || {
            let seen = observations.fetch_add(1, Ordering::SeqCst);
            async move {
                Ok(if seen < 20 {
                    NativeProgress::Exited(service_exit(0))
                } else {
                    NativeProgress::Loaded
                })
            }
        },
    )
    .await
    .expect("an exit that waits out the respawn throttle keeps waiting");
    assert!(matches!(ready, Readiness::Ready(_)));
}

#[tokio::test]
async fn readiness_hands_a_blocked_job_to_the_approval_wait() {
    let readiness = await_runtime_readiness(
        Duration::from_secs(5),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async { Ok(NativeProgress::AwaitingApproval) },
    )
    .await
    .unwrap();
    assert!(matches!(readiness, Readiness::ApprovalRequired));
}

#[tokio::test]
async fn readiness_wait_is_bounded() {
    let error = await_runtime_readiness(
        Duration::from_millis(20),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async { Ok(NativeProgress::Loaded) },
    )
    .await
    .expect_err("a runtime that never becomes ready must time out");
    assert!(error
        .to_string()
        .contains("did not publish runtime readiness"));
}
