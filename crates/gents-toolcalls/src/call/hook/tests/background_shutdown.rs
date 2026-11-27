use super::background_executions::BackgroundShutdownReport;
use super::*;

/// Kills the test's own recorded group if an assertion unwinds before the
/// stop under test has run, without depending on that stop or on the
/// executor teardown this test exists to replace.
#[cfg(unix)]
struct SpawnedGroupGuard(gents_session::exec::ownership::ProcessIdentity);

#[cfg(unix)]
impl Drop for SpawnedGroupGuard {
    fn drop(&mut self) {
        use gents_session::exec::ownership::ProcessObservation;
        if self.0.observe() == ProcessObservation::Running {
            // SAFETY: the leader's recorded start time and group were just
            // re-observed, so the group is the one this test spawned.
            unsafe { libc::killpg(self.0.pgid, libc::SIGKILL) };
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn shutdown_terminates_a_live_background_process_group() {
    use crate::test_support::held_process::{HeldProcess, HOLD};
    use gents_session::exec::ownership::{ProcessObservation, KILL_GRACE};
    use gents_store::test_support::STALL_CEILING;
    use tokio_util::sync::CancellationToken;

    let directory = tempfile::tempdir().unwrap();
    let mut process = HeldProcess::new(directory.path()).await;

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
        format!("trap '' TERM; {HOLD}"),
    ];
    let worker = tokio::spawn(async move {
        let outcome =
            gents_session::exec::run_managed_exec(gents_session::exec::ManagedExecRequest {
                argv,
                cwd,
                deadline_at: None,
                cancellation_token: worker_token,
                max_output_bytes: 1024,
                stdin: Vec::new(),
                environment: None,
                tool_name: Some("gents-shutdown-probe".to_owned()),
                live_output: None,
                process_recorder: Some(recorder),
            })
            .await;
        drop(reservation);
        outcome
    });

    process.entered().await;
    let identity = tokio::time::timeout(STALL_CEILING, async {
        loop {
            let notified = registry.process_record_notified();
            if let Some(record) = registry.process_record("shutdown-call", "shutdown-doc") {
                return record.identity;
            }
            notified.await;
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

    let outcome = tokio::time::timeout(STALL_CEILING, worker)
        .await
        .expect("the stopped worker releases its execution")
        .expect("worker task joins");
    assert!(
        matches!(
            outcome,
            gents_session::exec::ManagedExecOutcome::Cancelled { .. }
        ),
        "the stop runs through the worker's own cancellation: {outcome:?}"
    );
}
