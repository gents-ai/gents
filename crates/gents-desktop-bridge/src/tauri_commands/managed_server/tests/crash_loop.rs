use super::*;

fn looping_restarts(watch: &mut CrashLoopWatch, sample: JobSample<'_>) -> Option<u64> {
    watch.observe(sample).map(|(_, restarts)| restarts)
}

#[test]
fn status_reports_a_crash_loop_only_relative_to_its_first_observation() {
    let mut watch = CrashLoopWatch::default();
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(7))),
        None
    );
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(8))),
        None
    );
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
        Some(2)
    );
    assert_eq!(looping_restarts(&mut watch, JobSample::Unloaded), None);
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(9))),
        None,
        "an unloaded job starts a new baseline"
    );
}

#[test]
fn a_job_that_dies_seconds_into_boot_is_a_crash_loop_across_running_samples() {
    let mut watch = CrashLoopWatch::default();
    let samples = [
        (JobSample::Running, None),
        (JobSample::Exited(&service_exit(3)), None),
        (JobSample::Running, None),
        (JobSample::Running, None),
        (JobSample::Exited(&service_exit(4)), None),
        (JobSample::Running, None),
        (JobSample::Exited(&service_exit(5)), Some(2)),
        (JobSample::Running, Some(2)),
        (JobSample::Running, Some(2)),
    ];
    for (index, (sample, expected)) in samples.into_iter().enumerate() {
        assert_eq!(
            looping_restarts(&mut watch, sample),
            expected,
            "sample {index}"
        );
    }
    let (exit, _) = watch.observe(JobSample::Running).expect("still looping");
    assert_eq!(exit.reason, "exited with code 78");

    assert_eq!(looping_restarts(&mut watch, JobSample::Ready), None);
    assert_eq!(
        looping_restarts(&mut watch, JobSample::Exited(&service_exit(6))),
        None,
        "readiness starts a new baseline"
    );
    assert_eq!(
        looping_restarts(
            &mut watch,
            JobSample::Exited(&gents_server::native_service::ServiceExit {
                reason: "exited normally".to_string(),
                restarts: 9,
                clean: true,
                code: Some(0),
            })
        ),
        None
    );
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
    assert!(status.error.unwrap().contains("exited with code 78"));
}

#[test]
fn a_linux_unit_the_supervisor_gave_up_on_is_failed_with_its_exit_cause() {
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
    assert!(error.contains("will not be restarted"), "{error}");

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
    )
    .await
    .expect_err("a crash loop fails");
    let error = error.to_string();
    assert!(error.contains("exited with code 78"), "{error}");
    assert!(
        error.contains("another program that is not a Gents Node"),
        "{error}"
    );
}

#[tokio::test]
async fn readiness_fails_with_the_cause_when_the_supervisor_gives_up() {
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
    )
    .await
    .expect_err("a unit the supervisor gave up on is not waited on");
    assert!(error.to_string().contains("start-limit-hit"), "{error}");
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
    )
    .await
    .expect_err("two new restarts beyond the first observation fail");
    assert!(error.to_string().contains("exited with code 78"), "{error}");
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
    assert!(message.contains("did:key:other"), "{message}");
    assert!(message.contains("/Users/test/other-home"), "{message}");
    assert!(
        message.contains("gents service stop --home /Users/test/other-home"),
        "{message}"
    );
    assert!(message.contains("--http-port"), "{message}");

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
    assert!(anonymous
        .message()
        .contains("does not advertise a Gents identity"));
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
    assert!(status.error.as_deref().unwrap().contains("did:key:other"));

    let stopped = gents_server::native_service::NativeServiceStatus {
        running: false,
        job_loaded: false,
        ..loaded
    };
    let mut status = status_from(&idle, None, Some(&stopped), None, None);
    project_foreign_port(&mut status, &foreign.message(), false);
    assert_eq!(status.state, ManagedServerState::Stopped);
    assert!(status.error.as_deref().unwrap().contains("<its home>"));

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
    )
    .await
    .expect_err("two restarts after the reset fail");
    assert!(
        error.to_string().contains("restarted 2 times in a row"),
        "{error}"
    );
    assert_eq!(observations.load(Ordering::SeqCst), 4);
}
