use super::*;

/// Kills the test's own recorded group if an assertion unwinds before the
/// stop under test has run, without depending on that stop or on the
/// executor teardown this test exists to replace.
#[cfg(unix)]
struct SpawnedGroupGuard(crate::tools::exec::ownership::ProcessIdentity);

#[cfg(unix)]
impl Drop for SpawnedGroupGuard {
    fn drop(&mut self) {
        use crate::tools::exec::ownership::ProcessObservation;
        if self.0.observe() == ProcessObservation::Running {
            // SAFETY: the leader's recorded start time and group were just
            // re-observed, so the group is the one this test spawned.
            unsafe { libc::killpg(self.0.pgid, libc::SIGKILL) };
        }
    }
}

/// Runtime shutdown stops the background processes the runtime still owns.
/// The child ignores `SIGTERM` and never exits on its own, so only the
/// identity-verified escalation inside the existing stop path can end it.
#[cfg(unix)]
#[tokio::test]
async fn shutdown_terminates_a_live_background_process_group() {
    use crate::tools::exec::ownership::{ProcessObservation, KILL_GRACE};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let gate = directory.path().join("gate");
    assert!(std::process::Command::new("mkfifo")
        .arg(&gate)
        .status()
        .unwrap()
        .success());

    let registry =
        BackgroundExecutionRegistry::default().with_process_records(directory.path().to_owned());
    let token = CancellationToken::new();
    let reservation = registry
        .reserve(
            "shutdown-call".to_owned(),
            "shutdown-doc".to_owned(),
            token.clone(),
        )
        .unwrap();
    let recorder = registry.process_recorder("shutdown-call", "shutdown-doc");
    let cwd = directory.path().to_owned();
    let worker_token = token.clone();
    let argv = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        r#"trap '' TERM; : > "$1"; read ignored < "$2""#.to_owned(),
        "gents-shutdown-probe".to_owned(),
        ready.to_string_lossy().into_owned(),
        gate.to_string_lossy().into_owned(),
    ];
    let worker = tokio::spawn(crate::tools::exec::ownership::scope_process_recorder(
        recorder,
        async move {
            let outcome =
                crate::tools::exec::run_managed_exec(crate::tools::exec::ManagedExecRequest {
                    argv,
                    cwd,
                    deadline_at: None,
                    cancellation_token: worker_token,
                    max_output_bytes: 1024,
                    stdin: Vec::new(),
                    environment: None,
                    tool_name: Some("gents-shutdown-probe".to_owned()),
                    live_output: None,
                })
                .await;
            drop(reservation);
            outcome
        },
    ));

    let identity = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if ready.exists() {
                if let Some(record) = registry.process_record("shutdown-call", "shutdown-doc") {
                    return record.identity;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the spawned child installs its TERM trap and records its identity");
    let guard = SpawnedGroupGuard(identity.clone());
    assert_eq!(identity.observe(), ProcessObservation::Running);

    let report = registry.shutdown().await;
    assert_eq!(
        identity.await_signalled_group_exit(KILL_GRACE).await,
        ProcessObservation::Exited,
        "shutdown must escalate past the ignored SIGTERM to end the group"
    );
    assert_eq!(
        report,
        BackgroundShutdownReport {
            stopped: 1,
            unstopped: 0
        }
    );
    assert!(token.is_cancelled());
    drop(guard);

    let outcome = tokio::time::timeout(Duration::from_secs(30), worker)
        .await
        .expect("the stopped worker releases its execution")
        .expect("worker task joins");
    assert!(
        matches!(
            outcome,
            crate::tools::exec::ManagedExecOutcome::Cancelled { .. }
        ),
        "the stop runs through the worker's own cancellation: {outcome:?}"
    );
}
