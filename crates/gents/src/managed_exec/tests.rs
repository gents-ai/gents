#[cfg(unix)]
use rapidhash::RapidHashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::lean_vocab_test::{
    assert_lean_contract_vocabulary_matches, assert_lean_transition_is_legal,
    assert_state_machine_contract_is_complete, LeanContractVocabulary,
};

#[test]
fn rust_managed_exec_state_vocabulary_matches_lean_model() {
    let rust_states = ManagedExecState::ALL
        .iter()
        .copied()
        .map(ManagedExecState::as_str)
        .collect::<Vec<_>>();
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ManagedExecState",
        rust_source: "ManagedExecState::ALL",
        rust_values: &rust_states,
    });
}

#[test]
fn managed_exec_state_machine_contract_is_complete() {
    assert_state_machine_contract_is_complete("ManagedExec");
    assert_lean_transition_is_legal("ManagedExec", "pendingSpawn", "running");
    assert_lean_transition_is_legal("ManagedExec", "running", "killSignaled");
    assert_lean_transition_is_legal("ManagedExec", "killSignaled", "killed");
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_deadline_kills_process_group() {
    let tool_name = "r3-soak-blocker";
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready");
    let child_ready = ready.clone();
    let handle = tokio::spawn(async move {
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "trap '' TERM; printf started; : > \"$1\"; while :; do sleep 60; done".to_string(),
                "managed-deadline".to_string(),
                child_ready.to_str().unwrap().to_string(),
            ],
            cwd: PathBuf::from("/"),
            deadline_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            cancellation_token: CancellationToken::new(),
            max_output_bytes: 1024,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some(tool_name.to_string()),
            live_output: None,
        })
        .await
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    assert_eq!(snapshot.tool_name.as_deref(), Some(tool_name));
    assert_eq!(snapshot.argv0, "/bin/sh");
    chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
        .expect("active executor snapshot must expose RFC3339 started_at");

    tokio::time::timeout(Duration::from_secs(30), async {
        while !ready.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the command installs its signal handler and writes output before the deadline");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::time::resume();
    let outcome = handle.await.expect("managed exec task should join");

    match outcome {
        ManagedExecOutcome::TimedOut { stdout, kill, .. } => {
            assert!(String::from_utf8_lossy(&stdout).contains("started"));
            assert!(kill.term_signal_sent);
            assert!(kill.kill_signal_sent);
            assert!(kill.reaped);
        }
        other => panic!("expected timeout outcome, got {other:?}"),
    }
    assert!(
        crate::active_native_executors()
            .into_iter()
            .all(|snapshot| snapshot.tool_name.as_deref() != Some(tool_name)),
        "timed-out native executor snapshot must clear after reap"
    );

    let next = tokio::time::timeout(
        Duration::from_secs(30),
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "printf next".to_string(),
            ],
            cwd: PathBuf::from("/"),
            deadline_at: None,
            cancellation_token: CancellationToken::new(),
            max_output_bytes: 1024,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some("r3-soak-next".to_string()),
            live_output: None,
        }),
    )
    .await
    .expect("the next command must complete after its predecessor was reaped");

    match next {
        ManagedExecOutcome::Exited { stdout, .. } => assert_eq!(stdout, b"next"),
        other => panic!("expected next managed exec to run after timeout, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_cancellation_kills_process_group() {
    let tool_name = "r3-cancel-blocker";
    let token = CancellationToken::new();
    let child_token = token.clone();
    let handle = tokio::spawn(async move {
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "while :; do sleep 1; done".to_string(),
            ],
            cwd: PathBuf::from("/"),
            deadline_at: None,
            cancellation_token: child_token,
            max_output_bytes: 1024,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some(tool_name.to_string()),
            live_output: None,
        })
        .await
    });
    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    // SAFETY: getpgid observes the still-running managed child without changing it.
    assert_eq!(unsafe { libc::getpgid(snapshot.pid) }, snapshot.pid);
    let snapshot_id = snapshot.id;
    let first_age = snapshot.age_ms;
    tokio::time::sleep(Duration::from_millis(25)).await;
    let aged_snapshot = crate::active_native_executors()
        .into_iter()
        .find(|snapshot| snapshot.id == snapshot_id)
        .expect("executor should remain visible until cancellation");
    assert!(
        aged_snapshot.age_ms >= first_age,
        "executor age must not move backwards"
    );

    token.cancel();

    match handle.await.expect("managed exec task should join") {
        ManagedExecOutcome::Cancelled { kill, .. } => {
            assert!(kill.term_signal_sent);
            assert!(kill.reaped);
        }
        other => panic!("expected cancelled outcome, got {other:?}"),
    }
}

#[cfg(windows)]
#[tokio::test]
async fn managed_exec_deadline_kills_job_object() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("deadline-grandchild.pid");
    let tool_name = "r3-windows-job-deadline";
    let handle = tokio::spawn({
        let cwd = temp.path().to_path_buf();
        let argv = windows_grandchild_argv(&pid_file);
        async move {
            run_managed_exec(ManagedExecRequest {
                argv,
                cwd,
                deadline_at: Some(Utc::now() + chrono::Duration::seconds(8)),
                cancellation_token: CancellationToken::new(),
                max_output_bytes: 1024,
                stdin: Vec::new(),
                environment: None,
                tool_name: Some(tool_name.to_string()),
                live_output: None,
            })
            .await
        }
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    let grandchild_pid = wait_for_windows_grandchild_pid(&pid_file).await;

    match handle.await.expect("managed exec task should join") {
        ManagedExecOutcome::TimedOut { kill, .. } => {
            assert!(kill.kill_signal_sent);
            assert!(kill.reaped);
        }
        other => panic!("expected timeout outcome, got {other:?}"),
    }

    assert_windows_process_exited(grandchild_pid).await;
}

#[cfg(windows)]
#[tokio::test]
async fn managed_exec_cancellation_kills_job_object() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("cancel-grandchild.pid");
    let tool_name = "r3-windows-job-cancel";
    let token = CancellationToken::new();
    let child_token = token.clone();
    let handle = tokio::spawn({
        let cwd = temp.path().to_path_buf();
        let argv = windows_grandchild_argv(&pid_file);
        async move {
            run_managed_exec(ManagedExecRequest {
                argv,
                cwd,
                deadline_at: None,
                cancellation_token: child_token,
                max_output_bytes: 1024,
                stdin: Vec::new(),
                environment: None,
                tool_name: Some(tool_name.to_string()),
                live_output: None,
            })
            .await
        }
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    let grandchild_pid = wait_for_windows_grandchild_pid(&pid_file).await;
    token.cancel();

    match handle.await.expect("managed exec task should join") {
        ManagedExecOutcome::Cancelled { kill, .. } => {
            assert!(kill.kill_signal_sent);
            assert!(kill.reaped);
        }
        other => panic!("expected cancelled outcome, got {other:?}"),
    }

    assert_windows_process_exited(grandchild_pid).await;
}

#[cfg(any(unix, windows))]
async fn wait_for_native_executor(tool_name: &str) -> crate::NativeExecutorStatus {
    let timeout_at = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(snapshot) = crate::active_native_executors()
            .into_iter()
            .find(|snapshot| snapshot.tool_name.as_deref() == Some(tool_name))
        {
            return snapshot;
        }
        if Instant::now() >= timeout_at {
            panic!("timed out waiting for active native executor snapshot for {tool_name}");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[cfg(windows)]
fn windows_grandchild_argv(pid_file: &std::path::Path) -> Vec<String> {
    let pid_file = powershell_single_quoted(pid_file);
    let script = format!(
        "$p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 30\"' -PassThru; Set-Content -Path {pid_file} -Value $p.Id; Write-Output \"grandchild=$($p.Id)\"; Start-Sleep -Seconds 30"
    );
    vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        script,
    ]
}

#[cfg(windows)]
fn powershell_single_quoted(path: &std::path::Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

#[cfg(windows)]
async fn wait_for_windows_grandchild_pid(pid_file: &std::path::Path) -> u32 {
    let timeout_at = Instant::now() + Duration::from_secs(6);
    loop {
        if let Ok(contents) = std::fs::read_to_string(pid_file) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                return pid;
            }
        }
        if Instant::now() >= timeout_at {
            panic!(
                "timed out waiting for managed exec grandchild pid file {}",
                pid_file.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(windows)]
async fn assert_windows_process_exited(pid: u32) {
    let timeout_at = Instant::now() + Duration::from_secs(2);
    loop {
        match windows_process_is_running(pid) {
            Ok(false) => return,
            Ok(true) => {}
            Err(error) => panic!("checking Windows process {pid} failed: {error}"),
        }
        if Instant::now() >= timeout_at {
            panic!("managed exec grandchild process {pid} survived job termination");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[cfg(windows)]
fn windows_process_is_running(pid: u32) -> std::io::Result<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const SYNCHRONIZE: u32 = 0x0010_0000;

    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
    if handle.is_null() {
        return Ok(false);
    }
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    unsafe {
        CloseHandle(handle);
    }

    match wait {
        WAIT_OBJECT_0 => Ok(false),
        WAIT_TIMEOUT => Ok(true),
        _ => Err(std::io::Error::last_os_error()),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_delivers_stdin_and_closes_the_pipe() {
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec!["/bin/sh".to_string(), "-c".to_string(), "wc -c".to_string()],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::seconds(10)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 1024,
        stdin: b"hello managed stdin".to_vec(),
        environment: None,
        tool_name: Some("stdin-passthrough".to_string()),
        live_output: None,
    })
    .await;

    match outcome {
        ManagedExecOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            let text = String::from_utf8_lossy(&stdout);
            assert_eq!(
                text.trim().parse::<u64>().expect("byte count"),
                19,
                "the child must observe exactly the supplied stdin bytes: {text:?}"
            );
        }
        other => panic!("expected exited outcome, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_environment_replaces_the_inherited_environment() {
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec!["/usr/bin/env".to_string()],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::seconds(10)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 4096,
        stdin: Vec::new(),
        environment: Some(RapidHashMap::from_iter([(
            "TEST_SCOPED_VAR".to_string(),
            "scoped-only".to_string(),
        )])),
        tool_name: Some("env-scope".to_string()),
        live_output: None,
    })
    .await;
    match outcome {
        ManagedExecOutcome::Exited { code, stdout, .. } => {
            assert_eq!(code, Some(0));
            assert!(
                stdout == b"TEST_SCOPED_VAR=scoped-only\n",
                "child environment must exactly match the supplied map"
            );
        }
        other => panic!("expected exited outcome, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_reports_spawn_failure_as_a_typed_outcome() {
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec!["/nonexistent/gents-missing-binary".to_string()],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::seconds(5)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 1024,
        stdin: Vec::new(),
        environment: None,
        tool_name: Some("spawn-failure".to_string()),
        live_output: None,
    })
    .await;
    match &outcome {
        ManagedExecOutcome::SpawnFailed { error } => {
            assert!(
                error.contains("No such file") || error.contains("no such file"),
                "spawn failure should carry the OS error: {error}"
            );
        }
        other => panic!("expected spawn-failed outcome, got {other:?}"),
    }
    assert!(
        crate::active_native_executors()
            .iter()
            .all(|snapshot| snapshot.tool_name.as_deref() != Some("spawn-failure")),
        "a failed spawn must not register an active executor snapshot"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_bounds_stdout_drain_after_direct_child_exit() {
    assert_bounded_capture_drain("stdout").await;
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_bounds_stderr_drain_after_direct_child_exit() {
    assert_bounded_capture_drain("stderr").await;
}

#[cfg(unix)]
/// The non-retained stream must reach EOF independently of unrelated tests'
/// forked children, which can inherit its writer until they exec.
async fn assert_bounded_capture_drain(retained_stream: &str) {
    const FIXTURE: &str = "GENTS_MANAGED_EXEC_DRAIN_FIXTURE";
    if std::env::var(FIXTURE).as_deref() != Ok(retained_stream) {
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!(
                    "managed_exec::tests::managed_exec_bounds_{retained_stream}_drain_after_direct_child_exit"
                ),
                "--nocapture",
            ])
            .env(FIXTURE, retained_stream)
            .output()
            .await
            .expect("run isolated pipe-retention fixture");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    use std::os::unix::ffi::OsStrExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("descendant.pid");
    let ready_file = temp.path().join("ready.fifo");
    let ready_name = std::ffi::CString::new(ready_file.as_os_str().as_bytes()).unwrap();
    // The CString keeps this NUL-terminated pointer valid through mkfifo.
    assert_eq!(
        unsafe { libc::mkfifo(ready_name.as_ptr(), 0o600) },
        0,
        "create readiness FIFO: {}",
        std::io::Error::last_os_error()
    );
    let redirect = match retained_stream {
        "stdout" => "2>/dev/null",
        "stderr" => ">/dev/null",
        other => panic!("unexpected retained stream {other}"),
    };
    // Redirection runs in the descendant after fork. Wait for its acknowledgement
    // before parent exit so only the intended inherited writer remains open.
    let script = format!(
        "printf stdout-before; printf stderr-before >&2; \
         {{ exec {redirect}; printf 'ready\\n' > \"$2\"; exec sleep 30; }} & \
         echo $! > \"$1\"; IFS= read -r ready < \"$2\"; \
         [ \"$ready\" = ready ] || exit 8; exit 7"
    );
    let tool_name = format!("retained-{retained_stream}-pipe");

    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                script,
                "gents-drain-fixture".to_string(),
                pid_file.to_str().expect("PID path").to_string(),
                ready_file.to_str().expect("FIFO path").to_string(),
            ],
            cwd: temp.path().to_path_buf(),
            deadline_at: Some(Utc::now() + chrono::Duration::seconds(10)),
            cancellation_token: CancellationToken::new(),
            max_output_bytes: 1024,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some(tool_name.clone()),
            live_output: None,
        }),
    )
    .await;

    let descendant_pid = std::fs::read_to_string(&pid_file)
        .expect("descendant pid file")
        .trim()
        .parse::<i32>()
        .expect("descendant pid");
    unsafe {
        libc::kill(descendant_pid, libc::SIGKILL);
    }

    let outcome =
        outcome.expect("managed exec must not wait indefinitely for descendant-held pipes");
    match outcome {
        ManagedExecOutcome::Exited {
            code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        } => {
            assert_eq!(code, Some(7), "direct child status remains authoritative");
            assert_eq!(stdout, b"stdout-before");
            assert_eq!(stderr, b"stderr-before");
            assert_eq!(stdout_truncated, retained_stream == "stdout");
            assert_eq!(stderr_truncated, retained_stream == "stderr");
        }
        other => panic!("expected exited outcome, got {other:?}"),
    }
    assert!(
        crate::active_native_executors()
            .into_iter()
            .all(|snapshot| snapshot.tool_name.as_deref() != Some(&tool_name)),
        "direct-child exit must promptly clear the active executor"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_bounds_continuously_writing_descendant_after_direct_child_exit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let pid_file = temp.path().join("continuous-descendant.pid");
    let script = format!("yes x & echo $! > '{}'; exit 7", pid_file.display());
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        run_managed_exec(ManagedExecRequest {
            argv: vec!["/bin/sh".to_string(), "-c".to_string(), script],
            cwd: temp.path().to_path_buf(),
            deadline_at: Some(Utc::now() + chrono::Duration::seconds(10)),
            cancellation_token: CancellationToken::new(),
            max_output_bytes: 1024,
            stdin: Vec::new(),
            environment: None,
            tool_name: Some("continuous-descendant-pipe".to_string()),
            live_output: None,
        }),
    )
    .await;
    let descendant_pid = std::fs::read_to_string(&pid_file)
        .expect("descendant pid file")
        .trim()
        .parse::<i32>()
        .expect("descendant pid");
    unsafe {
        libc::kill(descendant_pid, libc::SIGKILL);
    }
    let outcome = outcome.expect("continuous descendant must not hold capture forever");
    match outcome {
        ManagedExecOutcome::Exited {
            code,
            stdout,
            stdout_truncated,
            ..
        } => {
            assert_eq!(code, Some(7));
            assert_eq!(stdout.len(), 1024);
            assert!(stdout_truncated);
        }
        other => panic!("expected direct-child exited outcome, got {other:?}"),
    }
}

#[test]
fn rust_process_stop_outcome_vocabulary_matches_lean_model() {
    let values = ownership::ProcessStopOutcome::ALL
        .iter()
        .copied()
        .map(ownership::ProcessStopOutcome::as_str)
        .collect::<Vec<_>>();
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "ProcessStopOutcome",
        rust_source: "ProcessStopOutcome::ALL",
        rust_values: &values,
    });
}

#[test]
fn rust_cancel_process_reply_vocabulary_matches_lean_model() {
    let values = ownership::CancelProcessReply::ALL
        .iter()
        .copied()
        .map(ownership::CancelProcessReply::as_str)
        .collect::<Vec<_>>();
    assert_lean_contract_vocabulary_matches(LeanContractVocabulary {
        domain: "CancelProcessReply",
        rust_source: "CancelProcessReply::ALL",
        rust_values: &values,
    });
}

#[cfg(unix)]
#[tokio::test]
async fn generated_process_stop_cases_drive_host_owner() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::{ProcessIdentity, ProcessObservation, ProcessStopOutcome};

    let observation = |name: &str| {
        ProcessObservation::ALL
            .into_iter()
            .find(|observation| observation.as_str() == name)
            .unwrap_or_else(|| panic!("unknown Lean process observation {name}"))
    };
    let cases = crate::lean_vocab_test::lean_process_stop_cases();
    assert_eq!(cases.len(), 15);
    for case in cases.iter().filter(|case| !case.live_evidence.is_empty()) {
        let evidence = match case.live_evidence.as_str() {
            "never_spawned" => ownership::LiveProcessEvidence::NeverSpawned,
            "spawned_unverified" => ownership::LiveProcessEvidence::SpawnedUnverified,
            "exited" => ownership::LiveProcessEvidence::Exited,
            other => panic!("unknown live process evidence {other}"),
        };
        assert_eq!(
            ownership::retire_stop_receipt(false),
            case.retire_before_commit
        );
        assert_eq!(
            ownership::retire_stop_receipt(true),
            case.retire_after_commit
        );
        for (intent, expected) in [
            (
                ownership::NativeTerminalIntent::Completed,
                case.settle_completed,
            ),
            (
                ownership::NativeTerminalIntent::Cancelled,
                case.settle_cancelled,
            ),
            (
                ownership::NativeTerminalIntent::TimedOut,
                case.settle_timed_out,
            ),
            (ownership::NativeTerminalIntent::Failed, case.settle_failed),
            (
                ownership::NativeTerminalIntent::Panicked,
                case.settle_panicked,
            ),
        ] {
            assert_eq!(
                ownership::may_settle_native(intent, evidence),
                expected,
                "{} {intent:?}",
                case.name
            );
        }
        assert_eq!(
            ownership::settle_live_cancel(case.worker_released, evidence).as_str(),
            case.outcome,
            "{}",
            case.name
        );
    }
    for case in cases {
        let before = observation(&case.before);
        let after = observation(&case.after);
        let outcome = ProcessStopOutcome::from_observations(before, after);
        assert_eq!(outcome.as_str(), case.outcome, "{}", case.name);
        assert_eq!(before.may_terminate(), case.may_terminate, "{}", case.name);
        assert_eq!(
            after.retain_process_record(),
            case.retain_record,
            "{}",
            case.name
        );
        assert_eq!(
            outcome.cancel_reply().as_str(),
            case.cancel_reply,
            "{}",
            case.name
        );
    }
    let case = |before: ProcessObservation, after: ProcessObservation| {
        cases
            .iter()
            .find(|case| case.before == before.as_str() && case.after == after.as_str())
            .expect("generated observation pair")
    };

    let process = OwnedTestProcess::spawn(None).await;
    assert_eq!(process.identity.observe(), ProcessObservation::Running);
    let (before, after) = process.identity.stop().await;
    let stopped = case(before, after);
    assert_eq!(stopped.outcome, "stopped");
    assert_eq!(stopped.cancel_reply, "cancelled");
    assert_eq!(process.identity.observe(), ProcessObservation::Exited);

    let (before, after) = process.identity.stop().await;
    let exited = case(before, after);
    assert_eq!(exited.outcome, "alreadyExited");
    assert!(!exited.may_terminate);
    process.finish().await;

    let process = OwnedTestProcess::spawn(None).await;
    let reused = ProcessIdentity {
        start: format!("{}-reused", process.identity.start),
        ..process.identity.clone()
    };
    let (before, after) = reused.stop().await;
    let unowned = case(before, after);
    assert_eq!(unowned.outcome, "notOwned");
    assert_eq!(unowned.cancel_reply, "lost");
    assert_eq!(
        process.identity.observe(),
        ProcessObservation::Running,
        "an unproven identity must not be signalled"
    );
    process.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn durable_process_record_survives_a_fresh_store() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::{ProcessObservation, ProcessRecordStore};

    let dir = tempfile::tempdir().unwrap();
    let registry = crate::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    let process =
        OwnedTestProcess::spawn(Some(registry.process_recorder("spawned:tool", "tool-doc"))).await;
    let restarted = ProcessRecordStore::Durable(dir.path().to_path_buf());
    let record = restarted.read("tool-doc").expect("durable record");
    assert_eq!(record.tool_call_doc_id, "tool-doc");
    assert_eq!(record.identity, process.identity);
    assert_eq!(record.identity.observe(), ProcessObservation::Running);
    assert!(registry
        .process_record("spawned:tool", "other-doc")
        .is_none());
    restarted.remove("tool-doc");
    assert!(restarted.list().is_empty());
    process.finish().await;
}

#[cfg(unix)]
#[tokio::test]
async fn reused_call_ids_keep_distinct_durable_process_owners_after_restart() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::ProcessObservation;
    let dir = tempfile::tempdir().unwrap();
    let registry = crate::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    let first = OwnedTestProcess::spawn(Some(registry.process_recorder("call", "first-doc"))).await;
    let sibling =
        OwnedTestProcess::spawn(Some(registry.process_recorder("call", "sibling-doc"))).await;
    let restarted = crate::hook::BackgroundExecutionRegistry::default()
        .with_process_records(dir.path().to_path_buf());
    assert_eq!(restarted.process_record_list().len(), 2);
    assert_eq!(
        restarted.stop_execution("call", "first-doc").await,
        ownership::ProcessStopOutcome::Stopped
    );
    assert_eq!(first.identity.observe(), ProcessObservation::Exited);
    assert_eq!(sibling.identity.observe(), ProcessObservation::Running);
    restarted.forget_process_record("call", "first-doc");
    assert!(restarted.process_record("call", "sibling-doc").is_some());
    assert_eq!(
        restarted.stop_execution("call", "sibling-doc").await,
        ownership::ProcessStopOutcome::Stopped
    );
    first.finish().await;
    sibling.finish().await;
}

#[cfg(unix)]
/// Unrelated fork-based spawns can retain these pipe writers until exec. Isolate
/// complete-output stress from writers that can outlast the bounded drain.
#[test]
fn concurrent_managed_commands_retain_complete_output() {
    const FIXTURE: &str = "GENTS_MANAGED_EXEC_OUTPUT_FIXTURE";
    if std::env::var_os(FIXTURE).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "managed_exec::tests::concurrent_managed_commands_retain_complete_output",
                "--nocapture",
            ])
            .env(FIXTURE, "1")
            .status()
            .unwrap();
        assert!(status.success(), "isolated command-output stress failed");
        return;
    }
    let workers: Vec<_> = (0..16)
        .map(|_| {
            std::thread::spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(async {
                        for _ in 0..16 {
                            let outcome = run_managed_exec(ManagedExecRequest {
                                argv: vec!["/bin/sh".into(), "-c".into(), "printf captured".into()],
                                cwd: PathBuf::from("/"),
                                deadline_at: None,
                                cancellation_token: CancellationToken::new(),
                                max_output_bytes: 1024,
                                stdin: Vec::new(),
                                environment: None,
                                tool_name: None,
                                live_output: None,
                            })
                            .await;
                            match outcome {
                                ManagedExecOutcome::Exited {
                                    code,
                                    stdout,
                                    stderr,
                                    stdout_truncated,
                                    stderr_truncated,
                                } => {
                                    assert_eq!(code, Some(0));
                                    assert_eq!(stdout, b"captured");
                                    assert!(stderr.is_empty());
                                    assert!(
                                        !stdout_truncated && !stderr_truncated,
                                        "stdout_truncated={stdout_truncated}, stderr_truncated={stderr_truncated}"
                                    );
                                }
                                other => panic!("expected complete capture, got {other:?}"),
                            }
                        }
                    });
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
}
