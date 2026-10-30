use super::*;

#[tokio::test]
async fn start_waits_for_approval_without_the_lifecycle_lock_then_launches() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(true)].into()),
        readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:a"))].into()),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let (ready, _lifecycle) = launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .ok()
        .expect("start continues after approval");
    assert_eq!(ready.node_did.as_deref(), Some("did:key:a"));
    assert_eq!(
        launch
            .approval_waits
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    assert_eq!(launch.starts(), 1);
}

#[tokio::test]
async fn adopting_a_job_still_booting_at_the_bound_reports_it_booting() {
    let (_temp, state) = orchestration_state();
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let adopted = adopt_booting_runtime(&state, &token, lifecycle, async {
        Err(RuntimeStillBooting {
            waited: Duration::from_secs(300),
        }
        .into())
    })
    .await;
    let Err(error) = settle_adoption(&state, &token, Path::new("/tmp/home"), adopted).await else {
        panic!("a job still booting does not adopt as ready");
    };
    assert_eq!(error.code, BridgeErrorCode::RuntimeStillBooting);
    let managed = state.managed_server.lock().await;
    assert!(
        managed.last_error.is_none(),
        "status keeps reporting it starting"
    );
    assert!(!managed.starting);
}

#[tokio::test]
async fn a_start_still_booting_is_reported_as_booting_not_failed() {
    let (_temp, state) = orchestration_state();
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let (_, typed) = settle_launch_failure(
        &state,
        &token,
        LaunchFailure {
            error: RuntimeStillBooting {
                waited: Duration::from_secs(300),
            }
            .into(),
            attempted_start: false,
            lifecycle: Some(lifecycle),
        },
        || async { panic!("a booting runtime is not rolled back") },
    )
    .await
    .expect("settled");
    assert_eq!(
        typed.expect("typed").code,
        BridgeErrorCode::RuntimeStillBooting
    );
    let managed = state.managed_server.lock().await;
    assert!(
        managed.last_error.is_none(),
        "status keeps reporting it starting"
    );
    assert!(!managed.starting);
}

#[tokio::test]
async fn a_start_still_booting_at_the_bound_is_not_rolled_back() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        still_booting: true,
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let Err(failure) = launch_managed_server(&state, &token, lifecycle, &launch).await else {
        panic!("a runtime still booting at the bound fails the start");
    };
    assert_eq!(launch.starts(), 1);
    assert!(
        !failure.attempted_start,
        "a job that may be migrating is left running"
    );
    assert!(failure.error.is::<RuntimeStillBooting>());
}

#[tokio::test]
async fn a_start_refused_for_approval_is_retried_once_after_approval() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(false), Ok(true)].into()),
        starts: std::sync::Mutex::new(
            [
                Err(BridgeError::untyped("blocked by Login Items")),
                Ok(Launched::Started),
            ]
            .into(),
        ),
        readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:b"))].into()),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    assert!(launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .is_ok());
    assert_eq!(launch.starts(), 2);
    assert_eq!(
        launch
            .approval_waits
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
}

#[tokio::test]
async fn a_start_macos_keeps_refusing_is_retried_after_each_approval_wait() {
    let (_temp, state) = orchestration_state();
    let refused = || Ok(Launched::ApprovalPending("Bootstrap failed: 5".to_string()));
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(false), Ok(true), Ok(true), Ok(true)].into()),
        starts: std::sync::Mutex::new(
            [refused(), refused(), refused(), Ok(Launched::Started)].into(),
        ),
        readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("d:k:x"))].into()),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    assert!(launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .is_ok());
    assert_eq!(launch.starts(), 4);
    assert_eq!(
        launch
            .approval_waits
            .load(std::sync::atomic::Ordering::SeqCst),
        3
    );
}

#[tokio::test]
async fn a_refusal_approval_polling_cannot_observe_fails_with_its_detail() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        starts: std::sync::Mutex::new(
            [Ok(Launched::ApprovalPending(
                "Bootstrap failed: 5: Input/output error".to_string(),
            ))]
            .into(),
        ),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let failure = tokio::time::timeout(
        Duration::from_secs(1),
        launch_managed_server(&state, &token, lifecycle, &launch),
    )
    .await
    .expect("an unobservable refusal is not retried in a loop")
    .expect_err("the start fails");
    assert!(
        failure.error.to_string().contains("Input/output error"),
        "{}",
        failure.error
    );
    assert_eq!(launch.starts(), 1);
    assert_eq!(
        launch
            .approval_waits
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[test]
fn a_runtime_still_migrating_keeps_the_crash_loop_evidence() {
    let mut watch = CrashLoopWatch::default();
    for restarts in [1, 2, 3] {
        watch.observe(job_sample(
            &PortReadiness::NotListening,
            true,
            Some(&service_exit(restarts)),
        ));
        assert!(
            matches!(
                job_sample(&PortReadiness::Booting, true, None),
                JobSample::Running
            ),
            "a booting runtime samples as running"
        );
        watch.observe(job_sample(&PortReadiness::Booting, true, None));
    }
    assert_eq!(
        watch
            .observe(job_sample(&PortReadiness::Booting, true, None))
            .map(|(_, restarts)| restarts),
        Some(2)
    );
    assert!(watch
        .observe(job_sample(
            &PortReadiness::Ready(ready_status("did:key:ready")),
            true,
            None
        ))
        .is_none());
}

#[test]
fn a_refused_start_of_an_unregistered_item_is_reported_as_awaiting_approval() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: false,
        enabled: false,
        requires_approval: false,
        background_item: gents_server::native_service::BackgroundItemStatus::NotRegistered,
        failed: false,
        detail: None,
    };
    let fresh = ManagedServerRuntimeState::default();
    assert!(!status_from(&fresh, None, Some(&native), None, None).approval_required);
    let refused = ManagedServerRuntimeState {
        approval_refused: true,
        ..Default::default()
    };
    assert!(status_from(&refused, None, Some(&native), None, None).approval_required);
}

#[tokio::test]
async fn a_new_start_forgets_an_earlier_refusal() {
    let (_temp, state) = orchestration_state();
    state.managed_server.lock().await.approval_refused = true;
    let _token = begin_start_wait(&state).await;
    assert!(!state.managed_server.lock().await.approval_refused);
}

#[tokio::test]
async fn a_failed_approval_check_keeps_the_original_start_error() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new(
            [
                Ok(false),
                Err(BridgeError::untyped("launchctl unavailable")),
            ]
            .into(),
        ),
        starts: std::sync::Mutex::new([Err(BridgeError::untyped("kickstart failed"))].into()),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let failure = launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .expect_err("start fails");
    assert!(failure.error.to_string().contains("kickstart failed"));
    assert!(failure.attempted_start);
    assert!(failure.lifecycle.is_some());
}

#[tokio::test]
async fn a_job_blocked_after_launch_waits_for_approval_and_launches_again() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        readiness: std::sync::Mutex::new(
            [
                Readiness::ApprovalRequired,
                Readiness::Ready(ready_status("did:key:c")),
            ]
            .into(),
        ),
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    assert!(launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .is_ok());
    assert_eq!(launch.starts(), 2);
}

#[tokio::test]
async fn stop_during_the_approval_wait_returns_promptly_and_nothing_launches() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(true)].into()),
        approval_blocks: true,
        ..Default::default()
    };
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let start = launch_managed_server(&state, &token, lifecycle, &launch);
    let stop = async {
        launch.waiting.notified().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            cancel_start_wait(&state).await;
            drop(state.managed_server_lifecycle.lock().await);
        })
        .await
        .expect("stop takes the lifecycle lock while start waits");
    };
    let (failure, ()) =
        tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(start, stop) })
            .await
            .expect("start ends promptly after stop");
    let failure = failure.expect_err("a cancelled start fails");
    assert!(failure.error.to_string().contains("cancelled"));
    assert!(!failure.attempted_start);
    assert_eq!(launch.starts(), 0);
    assert!(token.is_cancelled());
    let managed = state.managed_server.lock().await;
    assert!(!managed.starting);
    assert!(managed.start_wait.is_none());
    assert!(managed.last_error.is_none());
}

#[tokio::test]
async fn restart_during_the_readiness_wait_takes_over_without_a_rollback() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch::default();
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let start = launch_managed_server(&state, &token, lifecycle, &launch);
    let restart = async {
        launch.waiting.notified().await;
        tokio::time::timeout(Duration::from_secs(1), async {
            cancel_start_wait(&state).await;
            state.managed_server_lifecycle.lock().await
        })
        .await
        .expect("restart takes the lifecycle lock while start waits")
    };
    let (failure, restart_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(start, restart)
    })
    .await
    .expect("start ends promptly after restart");
    let failure = failure.expect_err("the superseded start fails");
    assert!(failure.attempted_start);
    assert!(failure.lifecycle.is_none(), "restart owns the lifecycle");
    assert!(
        token.is_cancelled(),
        "the start must not roll back the restarted job"
    );
    assert_eq!(launch.starts(), 1);
    drop(restart_lifecycle);
}

#[tokio::test]
async fn a_booting_runtime_is_adopted_without_holding_the_lifecycle_lock() {
    let (_temp, state) = orchestration_state();
    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let adopt = adopt_booting_runtime(&state, &token, lifecycle, async {
        ready_rx.await.unwrap();
        Ok(Readiness::Ready(ready_status("did:key:booted")))
    });
    let observer = async {
        tokio::task::yield_now().await;
        drop(
            tokio::time::timeout(
                Duration::from_secs(1),
                state.managed_server_lifecycle.lock(),
            )
            .await
            .expect("the lock is free while the runtime boots"),
        );
        ready_tx.send(()).unwrap();
    };
    let (adopted, ()) = tokio::join!(adopt, observer);
    match adopted.unwrap() {
        BootOutcome::Ready(ready, _lifecycle) => {
            assert_eq!(ready.node_did.as_deref(), Some("did:key:booted"))
        }
        BootOutcome::NeedsLaunch(..) => panic!("a ready runtime is adopted"),
    }

    let token = begin_start_wait(&state).await;
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let blocked = adopt_booting_runtime(&state, &token, lifecycle, async {
        Ok(Readiness::ApprovalRequired)
    })
    .await
    .unwrap();
    assert!(matches!(blocked, BootOutcome::NeedsLaunch(..)));
}

#[test]
fn a_crash_looping_job_is_reported_failed_with_its_exit_reason() {
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
    let idle = ManagedServerRuntimeState::default();
    let status = status_from(&idle, None, Some(&native), Some(&service_exit(7)), None);
    assert_eq!(
        status.state,
        ManagedServerState::Starting,
        "a high restart count from earlier failures is not a crash loop by itself"
    );
    let exit = service_exit(9);
    let status = status_from(&idle, None, Some(&native), Some(&exit), Some((&exit, 2)));
    assert_eq!(status.state, ManagedServerState::Failed);
    let error = status.error.unwrap();
    assert!(error.contains("exited with code 78"));
    assert!(error.contains("restarted 2 times in a row"));

    let blocked = gents_server::native_service::NativeServiceStatus {
        requires_approval: true,
        ..native
    };
    let status = status_from(&idle, None, Some(&blocked), None, None);
    assert!(status.approval_required);
    assert_ne!(status.state, ManagedServerState::Starting);
}

#[tokio::test]
async fn pending_background_approval_is_published_then_startup_continues() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let checks = AtomicUsize::new(0);
    let published = AtomicUsize::new(0);
    await_background_approval(
        Duration::from_secs(5),
        Duration::from_millis(5),
        || {
            let attempt = checks.fetch_add(1, Ordering::SeqCst);
            async move { Ok(attempt < 3) }
        },
        || {
            published.fetch_add(1, Ordering::SeqCst);
            async {}
        },
    )
    .await
    .expect("startup continues once approval is granted");

    assert_eq!(checks.load(Ordering::SeqCst), 4);
    assert_eq!(published.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn background_approval_wait_is_bounded_and_names_login_items() {
    let error = await_background_approval(
        Duration::from_millis(20),
        Duration::from_millis(5),
        || async { Ok(true) },
        || async {},
    )
    .await
    .expect_err("approval that never arrives must time out");
    assert!(error.to_string().contains("Login Items"), "{error}");
}

#[test]
fn native_approval_state_is_part_of_the_managed_status() {
    let native = gents_server::native_service::NativeServiceStatus {
        installed: true,
        running: false,
        job_loaded: false,
        enabled: true,
        requires_approval: true,
        background_item: Default::default(),
        failed: false,
        detail: None,
    };
    let status = status_from(
        &ManagedServerRuntimeState::default(),
        None,
        Some(&native),
        None,
        None,
    );
    assert!(status.approval_required);
    let observed = project_external_status(ready_status("did:key:ready"), &native);
    assert!(
        observed.approval_required,
        "approval revoked while the runtime serves stays visible"
    );
}

#[tokio::test]
async fn stop_queued_behind_a_locked_start_cancels_it_before_launch() {
    let (_temp, state) = orchestration_state();
    let lifecycle = state.managed_server_lifecycle.lock().await;
    let token = begin_start_wait(&state).await;
    let stop = lock_lifecycle_superseding_start(&state);
    tokio::pin!(stop);
    assert!(
        futures_poll_once(stop.as_mut()).await.is_none(),
        "stop queues behind the locked start"
    );
    let launch = FakeLaunch::default();
    let failure = launch_managed_server(&state, &token, lifecycle, &launch)
        .await
        .expect_err("a start cancelled while it held the lock does not launch");
    assert!(failure.error.to_string().contains("stopped"));
    assert_eq!(launch.starts(), 0);
    drop(failure);
    let _stop_lifecycle = tokio::time::timeout(Duration::from_secs(1), stop)
        .await
        .expect("stop takes the lock");
    let managed = state.managed_server.lock().await;
    assert!(!managed.starting);
    assert!(managed.last_error.is_none());
}

#[tokio::test]
async fn a_newer_start_supersedes_a_waiting_one_with_its_own_message() {
    let (_temp, state) = orchestration_state();
    let first = begin_start_wait(&state).await;
    let second = begin_start_wait(&state).await;
    assert!(first.is_cancelled());
    assert!(!second.is_cancelled());
    assert_eq!(first.cancel_message(), START_SUPERSEDED);
    finish_start_wait(&state, &first).await;
    assert!(
        state.managed_server.lock().await.starting,
        "the superseded start leaves the newer start's state alone"
    );
    cancel_start_wait(&state).await;
    assert_eq!(second.cancel_message(), START_CANCELLED);
}

#[tokio::test]
async fn auto_start_changes_wait_for_a_start_in_progress() {
    let (_temp, state) = orchestration_state();
    let token = begin_start_wait(&state).await;
    assert!(ensure_no_start_waiting(&state).await.is_err());
    finish_start_wait(&state, &token).await;
    assert!(ensure_no_start_waiting(&state).await.is_ok());
}

#[tokio::test]
async fn stop_during_a_restart_readiness_wait_takes_the_lock_and_ends_the_restart() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch::default();
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    let token = begin_start_wait(&state).await;
    let restart = launch_managed_server(&state, &token, lifecycle, &launch);
    let stop = async {
        launch.waiting.notified().await;
        tokio::time::timeout(
            Duration::from_secs(1),
            lock_lifecycle_superseding_start(&state),
        )
        .await
        .expect("stop takes the lifecycle lock while restart waits")
    };
    let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(restart, stop)
    })
    .await
    .expect("restart ends promptly after stop");
    let failure = failure.expect_err("the stopped restart fails");
    assert!(failure.error.to_string().contains("stopped"));
    assert!(failure.attempted_start);
    assert!(failure.lifecycle.is_none());
    assert!(
        token.is_cancelled(),
        "restart must not clean up the stopped job"
    );
    assert!(!state.managed_server.lock().await.starting);
}

#[tokio::test]
async fn stop_queued_behind_a_locked_restart_cancels_its_later_wait() {
    let (_temp, state) = orchestration_state();
    let restart_lifecycle = state.managed_server_lifecycle.lock().await;
    let stop = lock_lifecycle_superseding_start(&state);
    tokio::pin!(stop);
    assert!(futures_poll_once(stop.as_mut()).await.is_none());

    let token = begin_start_wait(&state).await;
    let launch = FakeLaunch::default();
    let restart = launch_managed_server(&state, &token, restart_lifecycle, &launch);
    let stopper = async {
        tokio::time::timeout(Duration::from_secs(1), stop)
            .await
            .expect("stop gets the lock once restart waits")
    };
    let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(restart, stopper)
    })
    .await
    .expect("restart ends promptly");
    let failure = failure.expect_err("the restart is cancelled");
    assert!(failure.error.to_string().contains("stopped"));
    assert!(failure.lifecycle.is_none());
    assert!(token.is_cancelled());
    assert!(!state.managed_server.lock().await.starting);
}

async fn futures_poll_once<F>(future: F) -> Option<F::Output>
where
    F: Future + Unpin,
{
    let mut future = future;
    std::future::poll_fn(|cx| {
        std::task::Poll::Ready(match std::pin::Pin::new(&mut future).poll(cx) {
            std::task::Poll::Ready(output) => Some(output),
            std::task::Poll::Pending => None,
        })
    })
    .await
}

#[tokio::test]
async fn restart_waits_for_approval_without_the_lock_then_launches() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(true)].into()),
        approval_gated: true,
        readiness: std::sync::Mutex::new([Readiness::Ready(ready_status("did:key:r"))].into()),
        ..Default::default()
    };
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    let token = begin_start_wait(&state).await;
    let restart = launch_managed_server(&state, &token, lifecycle, &launch);
    let approve = async {
        launch.waiting.notified().await;
        assert_eq!(launch.starts(), 0, "nothing launches before approval");
        assert!(
            state.managed_server_lifecycle.try_lock().is_ok(),
            "the lock is free while restart waits for approval"
        );
        assert!(state.managed_server.lock().await.starting);
        launch.approval_release.notify_one();
    };
    let (restarted, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(restart, approve)
    })
    .await
    .expect("restart continues once approved");
    assert!(restarted.is_ok());
    assert_eq!(launch.starts(), 1);
}

#[tokio::test]
async fn stop_during_a_restart_approval_wait_cancels_it_cleanly() {
    let (_temp, state) = orchestration_state();
    let launch = FakeLaunch {
        approval: std::sync::Mutex::new([Ok(true)].into()),
        approval_blocks: true,
        ..Default::default()
    };
    let lifecycle = lock_lifecycle_superseding_start(&state).await;
    let token = begin_start_wait(&state).await;
    let restart = launch_managed_server(&state, &token, lifecycle, &launch);
    let stop = async {
        launch.waiting.notified().await;
        tokio::time::timeout(
            Duration::from_secs(1),
            lock_lifecycle_superseding_start(&state),
        )
        .await
        .expect("stop takes the lock during the approval wait")
    };
    let (failure, _stop_lifecycle) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(restart, stop)
    })
    .await
    .expect("restart ends promptly after stop");
    let failure = failure.expect_err("the stopped restart fails");
    assert!(failure.error.to_string().contains("stopped"));
    assert!(!failure.attempted_start);
    assert!(failure.lifecycle.is_none());
    assert_eq!(launch.starts(), 0);
    assert!(
        token.is_cancelled(),
        "a cancelled restart skips its cleanup stop"
    );
}
