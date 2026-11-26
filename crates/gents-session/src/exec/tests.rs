#[cfg(unix)]
use rapidhash::RapidHashMap;
use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::*;

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_deadline_kills_process_group() {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;

    let tool_name = "r3-soak-blocker";
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("ready.fifo");
    let ready_c = std::ffi::CString::new(ready.as_os_str().as_bytes()).unwrap();
    // SAFETY: `ready_c` is a NUL-terminated path valid through mkfifo.
    assert_eq!(unsafe { libc::mkfifo(ready_c.as_ptr(), 0o600) }, 0);
    struct UnblockFifo(PathBuf);
    impl Drop for UnblockFifo {
        fn drop(&mut self) {
            let _ = std::fs::OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(&self.0);
        }
    }
    let _unblock = UnblockFifo(ready.clone());
    let child_ready = ready.clone();
    let handle = tokio::spawn(async move {
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "trap '' TERM; printf started; printf 'ready\\n' > \"$1\"; while :; do sleep 60; done".to_string(),
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
            process_recorder: None,
        })
        .await
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    assert_eq!(snapshot.tool_name.as_deref(), Some(tool_name));
    assert_eq!(snapshot.argv0, "/bin/sh");
    chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
        .expect("active executor snapshot must expose RFC3399 started_at");

    let ready_reader = ready.clone();
    tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || std::fs::read_to_string(&ready_reader)),
    )
    .await
    .expect("the command installs its signal handler and writes output before the deadline")
    .expect("fifo reader task")
    .expect("read ready fifo");
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
        crate::exec::status::active_native_executors()
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
            process_recorder: None,
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
            process_recorder: None,
        })
        .await
    });
    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    // SAFETY: getpgid observes the still-running managed child without changing it.
    assert_eq!(unsafe { libc::getpgid(snapshot.pid) }, snapshot.pid);
    let snapshot_id = snapshot.id;
    let first_age = snapshot.age_ms;
    tokio::task::yield_now().await;
    let aged_snapshot = crate::exec::status::active_native_executors()
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
    let tool_name = "r3-windows-job-deadline";
    let (appender, mut output_rx) = crate::test_support::doubles::WatchAppender::new();
    let registry = gents_loop::live_output::LiveToolOutputRegistry::default();
    let writer = registry
        .canonical_writer_for(tool_name.to_string(), appender)
        .await;
    let handle = tokio::spawn({
        let cwd = temp.path().to_path_buf();
        let argv = windows_grandchild_argv();
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
                live_output: Some(writer),
                process_recorder: None,
            })
            .await
        }
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    let grandchild_pid = wait_for_windows_grandchild_pid(&mut output_rx).await;

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
    let tool_name = "r3-windows-job-cancel";
    let (appender, mut output_rx) = crate::test_support::doubles::WatchAppender::new();
    let registry = gents_loop::live_output::LiveToolOutputRegistry::default();
    let writer = registry
        .canonical_writer_for(tool_name.to_string(), appender)
        .await;
    let token = CancellationToken::new();
    let child_token = token.clone();
    let handle = tokio::spawn({
        let cwd = temp.path().to_path_buf();
        let argv = windows_grandchild_argv();
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
                live_output: Some(writer),
                process_recorder: None,
            })
            .await
        }
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    let grandchild_pid = wait_for_windows_grandchild_pid(&mut output_rx).await;
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
async fn wait_for_native_executor(tool_name: &str) -> crate::exec::status::NativeExecutorStatus {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let notified = super::process::active_executors_notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if let Some(snapshot) = crate::exec::status::active_native_executors()
            .into_iter()
            .find(|snapshot| snapshot.tool_name.as_deref() == Some(tool_name))
        {
            return snapshot;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            panic!("timed out waiting for active native executor snapshot for {tool_name}");
        }
    }
}

#[cfg(windows)]
fn windows_grandchild_argv() -> Vec<String> {
    let script = "$p = Start-Process -FilePath powershell.exe -ArgumentList '-NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 30\"' -PassThru; [Console]::WriteLine(\"grandchild=$($p.Id)\"); [Console]::Out.Flush(); Start-Sleep -Seconds 30";
    vec![
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-Command".to_string(),
        script.to_string(),
    ]
}

#[cfg(windows)]
fn parse_windows_grandchild_pid(output: &str) -> Option<u32> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("grandchild=")
            .and_then(|pid| pid.parse::<u32>().ok())
    })
}

#[cfg(windows)]
async fn wait_for_windows_grandchild_pid(
    output_rx: &mut tokio::sync::watch::Receiver<String>,
) -> u32 {
    let matched = tokio::time::timeout(
        Duration::from_secs(6),
        output_rx.wait_for(|text| parse_windows_grandchild_pid(text).is_some()),
    )
    .await
    .expect("timed out waiting for managed exec grandchild pid output")
    .expect("live output watch closed before grandchild pid arrived");
    parse_windows_grandchild_pid(&matched).expect("grandchild pid should parse")
}

#[cfg(windows)]
async fn assert_windows_process_exited(pid: u32) {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    const SYNCHRONIZE: u32 = 0x0010_0000;

    let exited = tokio::task::spawn_blocking(move || {
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid) };
        if handle.is_null() {
            return Ok(true);
        }
        let wait = unsafe { WaitForSingleObject(handle, 2_000) };
        unsafe {
            CloseHandle(handle);
        }
        match wait {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(std::io::Error::last_os_error()),
        }
    })
    .await
    .expect("waiting for Windows process exit should not panic")
    .unwrap_or_else(|error| panic!("checking Windows process {pid} failed: {error}"));
    assert!(
        exited,
        "managed exec grandchild process {pid} survived job termination"
    );
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
        process_recorder: None,
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
        process_recorder: None,
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
async fn managed_exec_spawn_outcome_matches_the_lean_model() {
    let cases = gents_lean_vocab::families::managed_exec_spawn_outcome::cases();
    for case in &cases {
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
            process_recorder: None,
        })
        .await;
        if let ManagedExecOutcome::SpawnFailed { error } = &outcome {
            assert!(
                error.contains("No such file") || error.contains("no such file"),
                "spawn failure should carry the OS error: {error}"
            );
        }
        let state = match outcome {
            ManagedExecOutcome::SpawnFailed { .. } => "spawnFailed",
            _ => "running",
        };
        let executor_registered = crate::exec::status::active_native_executors()
            .iter()
            .any(|snapshot| snapshot.tool_name.as_deref() == Some("spawn-failure"));
        case.check(
            gents_lean_vocab::families::managed_exec_spawn_outcome::SpawnOutput {
                state: state.to_string(),
                executor_registered,
            },
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_bounds_stdout_drain_after_direct_child_exit() {
    assert_bounded_capture_drain("stdout").await;
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
                    "exec::tests::managed_exec_bounds_{retained_stream}_drain_after_direct_child_exit"
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
            process_recorder: None,
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
        crate::exec::status::active_native_executors()
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
            process_recorder: None,
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

#[cfg(unix)]
#[tokio::test]
async fn owned_process_group_stops_match_lean() {
    use ownership::test_support::OwnedTestProcess;
    use ownership::{CancelProcessReply, ProcessIdentity, ProcessObservation, ProcessStopOutcome};

    for case in gents_lean_vocab::families::process_stop_owned_process::cases::<
        CancelProcessReply,
        ProcessObservation,
        ProcessStopOutcome,
    >() {
        let process = OwnedTestProcess::spawn(None).await;
        assert_eq!(process.identity.observe(), ProcessObservation::Running);
        let (before, after) = match case.input.before {
            ProcessObservation::Running => {
                let observed = process.identity.stop().await;
                assert_eq!(process.identity.observe(), ProcessObservation::Exited);
                observed
            }
            ProcessObservation::Exited => {
                process.identity.stop().await;
                process.identity.stop().await
            }
            ProcessObservation::Unowned => {
                let reused = ProcessIdentity {
                    start: format!("{}-reused", process.identity.start),
                    ..process.identity.clone()
                };
                let observed = reused.stop().await;
                assert_eq!(
                    process.identity.observe(),
                    ProcessObservation::Running,
                    "an unproven identity must not be signalled"
                );
                observed
            }
        };
        assert_eq!(
            (before, after),
            (case.input.before, case.input.after),
            "{}",
            case.input.name
        );
        let outcome = ProcessStopOutcome::from_observations(before, after);
        case.check(
            gents_lean_vocab::families::process_stop_owned_process::OwnedProcessOutput {
                outcome,
                cancel_reply: outcome.cancel_reply(),
                may_terminate: before.may_terminate(),
            },
        );
        process.finish().await;
    }
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
                "exec::tests::concurrent_managed_commands_retain_complete_output",
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
                                process_recorder: None,
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

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_liveness_follows_the_lean_composed_model() {
    for case in gents_lean_vocab::families::contracts_managed_exec_liveness_exec::cases::<String>()
    {
        let cancellation_token = CancellationToken::new();
        let tool_name = format!("liveness-{}", case.input.name);
        let (script, deadline_at) = match case.input.trigger.as_str() {
            "deadline_elapsed" => (
                "printf partial; sleep 30",
                Some(Utc::now() + chrono::Duration::milliseconds(200)),
            ),
            "cancel_requested" => {
                let token = cancellation_token.clone();
                let cancel_tool_name = tool_name.clone();
                tokio::spawn(async move {
                    let _ = wait_for_native_executor(&cancel_tool_name).await;
                    token.cancel();
                });
                ("sleep 30", None)
            }
            "observe_exit_success" => ("exit 0", None),
            "observe_exit_failure" => ("exit 1", None),
            other => panic!("no managed exec scenario for trigger {other}"),
        };
        let outcome = tokio::time::timeout(
            Duration::from_secs(30),
            run_managed_exec(ManagedExecRequest {
                argv: vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()],
                cwd: PathBuf::from("/"),
                deadline_at,
                cancellation_token,
                max_output_bytes: 1024,
                stdin: Vec::new(),
                environment: None,
                tool_name: Some(tool_name),
                live_output: None,
                process_recorder: None,
            }),
        )
        .await
        .expect("a managed command reaches its outcome within the bound");
        let (state, kill_signal_required) = match &outcome {
            ManagedExecOutcome::TimedOut { kill, .. }
            | ManagedExecOutcome::Cancelled { kill, .. } => {
                let signaled = kill.term_signal_sent || kill.kill_signal_sent;
                (if signaled { "killSignaled" } else { "exited" }, signaled)
            }
            ManagedExecOutcome::Exited { .. } => ("exited", false),
            ManagedExecOutcome::SpawnFailed { .. } => ("spawnFailed", false),
        };
        case.check(
            gents_lean_vocab::families::contracts_managed_exec_liveness_exec::ManagedExecLivenessExecOutput {
                exec_state: state.to_owned(),
                kill_signal_required,
            },
        );
    }
}
