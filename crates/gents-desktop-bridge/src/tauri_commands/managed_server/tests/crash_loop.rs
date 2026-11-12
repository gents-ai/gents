use super::*;

fn looping_restarts(watch: &mut CrashLoopWatch, sample: JobSample<'_>) -> Option<u64> {
    watch.observe(sample).map(|(_, restarts)| restarts)
}

#[test]
fn a_crash_loop_seen_between_respawns_is_reported_failed_with_its_reason() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let mut watch = CrashLoopWatch::default();
    for restarts in [1, 2, 3] {
        watch.observe(JobSample::Exited(&service_exit(restarts)));
        watch.observe(JobSample::Running);
    }
    let looping = watch.observe(JobSample::Running);
    let status = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        Some(&native),
        None,
        looping.as_ref().map(|(exit, restarts)| (exit, *restarts)),
    );
    assert_eq!(status.state, ManagedServerState::Failed);
    assert!(status.error.unwrap().contains("exit code 78"));
}

#[test]
fn a_linux_unit_the_supervisor_gave_up_on_is_failed_with_the_supervisors_reason() {
    let failed = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: false,
        enabled: false,
        requires_approval: false,
        background_item: Default::default(),
        failed: true,
        detail: Some("failed".to_string()),
    };
    let exit = gents_server::native_service::ServiceExit {
        reason: "exit-code (exit status 1)".to_string(),
        restarts: 5,
        clean: false,
        code: None,
    };
    let idle = ManagedServerRuntimeState::default();
    let status = status_from(&idle, None, Some(&failed), Some(&exit), None);
    assert_eq!(status.state, ManagedServerState::Failed);
    let error = status.error.unwrap();
    assert!(error.contains("exit-code (exit status 1)"), "{error}");

    let starting = ManagedServerRuntimeState {
        starting: true,
        ..Default::default()
    };
    assert_eq!(
        status_from(&starting, None, Some(&failed), Some(&exit), None).state,
        ManagedServerState::Starting,
        "a new start owns its own outcome"
    );
}

#[tokio::test]
async fn a_crash_loop_behind_a_non_gents_listener_names_the_port() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let observations = AtomicUsize::new(0);
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(1),
        || async { Ok(PortReadiness::Occupied(9191)) },
        || {
            let seen = observations.fetch_add(1, Ordering::SeqCst) as u64;
            async move { Ok(NativeProgress::Exited(service_exit(seen))) }
        },
        || async { None },
    )
    .await
    .expect_err("a crash loop fails");
    let error = error.to_string();
    assert!(error.contains("exit code 78"), "{error}");
    assert!(error.contains("is in use by another app"), "{error}");
}

#[tokio::test]
async fn readiness_fails_with_the_supervisors_reason_when_it_gives_up() {
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async {
            Ok(NativeProgress::Failed(
                gents_server::native_service::ServiceExit {
                    reason: "start-limit-hit (exit status 1)".to_string(),
                    restarts: 5,
                    clean: false,
                    code: None,
                },
            ))
        },
        || async { None },
    )
    .await
    .expect_err("a unit the supervisor gave up on is not waited on");
    assert!(
        error
            .to_string()
            .contains("start-limit-hit (exit status 1)"),
        "{error}"
    );
}

#[tokio::test]
async fn readiness_counts_restarts_from_a_high_first_observation() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let observations = AtomicUsize::new(0);
    let counters = [7, 7, 8, 8, 8, 9];
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(1),
        || async { Ok(PortReadiness::NotListening) },
        || {
            let seen = observations.fetch_add(1, Ordering::SeqCst);
            let restarts = counters[seen.min(counters.len() - 1)];
            async move { Ok(NativeProgress::Exited(service_exit(restarts))) }
        },
        || async { None },
    )
    .await
    .expect_err("two new restarts beyond the first observation fail");
    assert!(error.to_string().contains("exit code 78"), "{error}");
    assert_eq!(
        observations.load(Ordering::SeqCst),
        6,
        "the exit at counter 8 kept waiting; counter 9 failed"
    );
}

#[tokio::test]
async fn a_clean_exit_of_the_loaded_job_is_not_waited_out() {
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(5),
        || async { Ok(PortReadiness::NotListening) },
        || async {
            Ok(NativeProgress::Exited(
                gents_server::native_service::ServiceExit {
                    reason: "exited normally".to_string(),
                    restarts: 0,
                    clean: true,
                    code: Some(0),
                },
            ))
        },
        || async { None },
    )
    .await
    .expect_err("a job that exited normally will not come back by itself");
    assert!(error.to_string().contains("exited normally"), "{error}");
}

#[test]
fn a_loaded_job_that_exited_cleanly_is_reported_stopped() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let clean = gents_server::native_service::ServiceExit {
        reason: "exited normally".to_string(),
        restarts: 0,
        clean: true,
        code: Some(0),
    };
    let status = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        Some(&native),
        Some(&clean),
        None,
    );
    assert_eq!(status.state, ManagedServerState::Stopped);
}

#[test]
fn outdated_managed_restart_is_rejected_before_native_mutation() {
    let outdated = PortReadiness::Outdated {
        version: Some("older".into()),
    };
    let error = ensure_selected_restart_compatible(true, &outdated).unwrap_err();
    assert_eq!(error.code, BridgeErrorCode::Unsupported);
    assert!(error.message.contains("service update"));
    ensure_selected_restart_compatible(false, &outdated).unwrap();
    ensure_selected_restart_compatible(true, &PortReadiness::NotListening).unwrap();
}

#[tokio::test]
async fn incompatible_managed_selection_is_preserved_when_readiness_fails() {
    let (_temp, state) = orchestration_state();
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let launch = FakeLaunch {
        readiness: std::sync::Mutex::new(
            [preserve_managed_selection(
                Readiness::Outdated {
                    message: "old package".into(),
                    selected: false,
                },
                true,
            )]
            .into(),
        ),
        ..Default::default()
    };
    let failure = launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .unwrap_err();
    assert!(failure.attempted_start && failure.preserve_runtime);
    let (message, _) = settle_launch_failure(&state, &token, failure, || async {
        panic!("an explicitly selected runtime must not be stopped for being older than this app")
    })
    .await
    .unwrap();
    assert!(message.contains("compatible packaged build"));
    assert!(state.managed_server.lock().await.start_wait.is_none());
}

#[tokio::test]
async fn a_failed_install_settles_the_restart_instead_of_leaking_its_wait() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_temp, state) = orchestration_state();
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    let token = begin_start_wait(&state).await;
    let launch = FakeLaunch::default();
    let failure = install_then_launch(
        &state,
        &token,
        lifecycle,
        async { Err(BridgeError::untyped("copying the Gents runtime failed")) },
        &launch,
    )
    .await
    .expect_err("a failed install fails the launch");
    assert!(!failure.attempted_start);
    assert_eq!(launch.starts(), 0);

    let cleaned = AtomicBool::new(false);
    let (message, _) = settle_launch_failure(&state, &token, failure, || async {
        cleaned.store(true, Ordering::SeqCst);
        Ok(())
    })
    .await
    .expect("an uncancelled failure is recorded");
    assert!(message.contains("copying the Gents runtime failed"));
    assert!(
        !cleaned.load(Ordering::SeqCst),
        "nothing was started to roll back"
    );
    let managed = state.managed_server.lock().await;
    assert!(!managed.starting);
    assert!(managed.start_wait.is_none());
    assert_eq!(managed.last_error.as_deref(), Some(message.as_str()));
    drop(managed);
    assert!(ensure_no_start_waiting(&state).await.is_ok());
}

#[test]
fn a_reset_restart_counter_restarts_the_crash_loop_baseline() {
    let mut watch = CrashLoopWatch::default();
    for restarts in [7, 8, 9] {
        watch.observe(JobSample::Exited(&service_exit(restarts)));
    }
    assert_eq!(looping_restarts(&mut watch, JobSample::Running), Some(2));
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(1))),
        None,
        "a lower counter is a reloaded job"
    );
    assert_eq!(watch.baseline, Some(1));
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(2))),
        None
    );
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(3))),
        Some(2)
    );
}

#[test]
fn any_runtime_on_the_port_of_a_fresh_home_is_foreign_and_named() {
    let payload = serde_json::json!({
        "node_did": "did:key:other",
        "node_name": "other",
        "home": "/Users/test/other-home",
        gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD: "ready",
    });
    let PortReadiness::Foreign(foreign) = classify_port_payload(9191, None, payload.clone()) else {
        panic!("a fresh home adopts no runtime");
    };
    let message = foreign.message();
    assert!(message.contains("started by hand"), "{message}");
    assert!(message.contains("/Users/test/other-home"), "{message}");

    assert!(matches!(
        classify_port_payload(9191, Some("did:key:mine"), payload.clone()),
        PortReadiness::Foreign(_)
    ));
    assert!(matches!(
        classify_port_payload(9191, Some("did:key:other"), payload),
        PortReadiness::Ready(_)
    ));

    let PortReadiness::Foreign(anonymous) =
        classify_port_payload(9191, Some("did:key:mine"), serde_json::json!({}))
    else {
        panic!("a listener without an identity is not this home's");
    };
    assert!(anonymous.message().contains("is in use by another app"));
}

#[test]
fn a_foreign_runtime_fails_a_loaded_job_and_names_itself_when_stopped() {
    let foreign = ForeignRuntime {
        port: 9191,
        node_did: Some("did:key:other".to_string()),
        home: None,
    };
    let loaded = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let idle = ManagedServerRuntimeState::default();
    let mut status = status_from(&idle, None, Some(&loaded), None, None);
    project_foreign_port(&mut status, &foreign.message(), false);
    assert_eq!(
        status.state,
        ManagedServerState::Failed,
        "a job that cannot bind its port is not waited on"
    );
    assert!(status.error.as_deref().unwrap().contains("started by hand"));

    let stopped = gents_server::native_service::NativeServiceStatus {
        running: false,
        job_loaded: false,
        ..loaded
    };
    let mut status = status_from(&idle, None, Some(&stopped), None, None);
    project_foreign_port(&mut status, &foreign.message(), false);
    assert_eq!(status.state, ManagedServerState::Stopped);
    assert!(status.error.as_deref().unwrap().contains("started by hand"));

    let mut status = status_from(&idle, None, Some(&stopped), None, None);
    project_foreign_port(&mut status, &foreign.message(), true);
    assert!(
        status.error.is_none(),
        "a start in progress reports it itself"
    );
}

#[test]
fn another_homes_runtime_never_blocks_stop_or_restart() {
    for initialized_did in [None, Some("did:key:mine")] {
        let port = classify_port_payload(
            9191,
            initialized_did,
            serde_json::json!({ "node_did": "did:key:other" }),
        );
        assert!(matches!(port, PortReadiness::Foreign(_)));
        for job_loaded in [false, true] {
            ensure_native_owns_running_endpoint(&port, job_loaded)
                .expect("stopping this home's job is never refused for another runtime");
        }
    }
}

#[tokio::test]
async fn readiness_restarts_its_baseline_when_the_counter_drops() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let observations = AtomicUsize::new(0);
    let counters = [7, 1, 2, 3];
    let error = await_runtime_readiness(
        Duration::from_secs(60),
        Duration::from_millis(1),
        || async { Ok(PortReadiness::NotListening) },
        || {
            let seen = observations.fetch_add(1, Ordering::SeqCst);
            let restarts = counters[seen.min(counters.len() - 1)];
            async move { Ok(NativeProgress::Exited(service_exit(restarts))) }
        },
        || async { None },
    )
    .await
    .expect_err("two restarts after the reset fail");
    assert!(error.to_string().contains("keeps exiting"), "{error}");
    assert_eq!(observations.load(Ordering::SeqCst), 4);
}

#[test]
fn a_crash_loop_names_the_last_error() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let exit = service_exit(1);
    let runtime = ManagedServerRuntimeState {
        last_exit_error: Some("the store is corrupt".to_string()),
        ..Default::default()
    };
    let status = status_from(&runtime, None, Some(&native), None, Some((&exit, 3)));
    assert_eq!(
        status.error.unwrap(),
        "The background node keeps exiting: the store is corrupt"
    );
}

#[test]
fn a_missing_store_key_is_left_to_the_repair() {
    let exit = gents_server::native_service::ServiceExit {
        reason: "exited with code 72".to_string(),
        restarts: 1,
        clean: false,
        code: Some(72),
    };
    let status = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        None,
        Some(&exit),
        None,
    );
    assert_eq!(status.error_code, None);
}

#[test]
fn a_foreign_gents_node_is_described_as_started_by_hand() {
    let foreign = ForeignRuntime {
        port: 9191,
        node_did: Some("did:key:other".to_string()),
        home: Some("/Users/test/other-home".to_string()),
    };
    assert_eq!(
        foreign.message(),
        "Port 9191 is used by a Gents node started by hand from /Users/test/other-home. Stop it, then try again."
    );
}

#[tokio::test]
async fn the_logged_error_reaches_the_crash_loop_status() {
    let home = tempfile::tempdir().unwrap();
    let desktop = tempfile::tempdir().unwrap();
    let log = gents_server::native_service::runtime_error_log(home.path());
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "starting\nError: the store is corrupt\n").unwrap();
    let last_exit_error = last_error_in(home.path().to_path_buf(), desktop.path()).await;
    assert_eq!(last_exit_error.as_deref(), Some("the store is corrupt"));

    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: true,
        job_loaded: true,
        enabled: true,
        requires_approval: false,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let exit = service_exit(1);
    let runtime = ManagedServerRuntimeState {
        last_exit_error,
        ..Default::default()
    };
    let status = status_from(&runtime, None, Some(&native), None, Some((&exit, 3)));
    assert_eq!(
        status.error.unwrap(),
        "The background node keeps exiting: the store is corrupt"
    );
}

#[tokio::test]
async fn the_old_desktop_log_is_the_fallback() {
    let home = tempfile::tempdir().unwrap();
    let desktop = tempfile::tempdir().unwrap();
    let log = crate::logging::runtime_error_log(desktop.path());
    std::fs::create_dir_all(log.parent().unwrap()).unwrap();
    std::fs::write(&log, "Error: from the old log\n").unwrap();
    assert_eq!(
        last_error_in(home.path().to_path_buf(), desktop.path())
            .await
            .as_deref(),
        Some("from the old log")
    );
}

#[test]
fn a_missing_store_key_is_not_a_store_refusal_of_the_observe_path() {
    let exit = |code| gents_server::native_service::ServiceExit {
        reason: format!("exited with code {code}"),
        restarts: 1,
        clean: false,
        code: Some(code),
    };
    assert!(store_refusal(&exit(72)).is_none());
    assert!(store_refusal(&exit(65)).is_some());
}
