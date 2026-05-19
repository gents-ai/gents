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
    let handle = tokio::spawn(async move {
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "echo started; trap '' TERM; sleep 5".to_string(),
            ],
            cwd: PathBuf::from("/"),
            deadline_at: Some(Utc::now() + chrono::Duration::milliseconds(500)),
            cancellation_token: CancellationToken::new(),
            max_output_bytes: 1024,
            stdin: Vec::new(),
            tool_name: Some(tool_name.to_string()),
        })
        .await
    });

    let snapshot = wait_for_native_executor(tool_name).await;
    assert!(snapshot.pid > 0, "active executor snapshot must expose pid");
    assert_eq!(snapshot.tool_name.as_deref(), Some(tool_name));
    assert!(
        snapshot.argv0.ends_with("sh"),
        "active executor snapshot must expose argv0, got {:?}",
        snapshot.argv0
    );
    chrono::DateTime::parse_from_rfc3339(&snapshot.started_at)
        .expect("active executor snapshot must expose RFC3339 started_at");
    let snapshot_id = snapshot.id;
    let first_age = snapshot.age_ms;
    tokio::time::sleep(Duration::from_millis(25)).await;
    let aged_snapshot = crate::active_native_executors()
        .into_iter()
        .find(|snapshot| snapshot.id == snapshot_id)
        .expect("executor should remain visible before deadline");
    assert!(
        aged_snapshot.age_ms >= first_age,
        "executor age must not move backwards"
    );

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

    let next = run_managed_exec(ManagedExecRequest {
        argv: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf next".to_string(),
        ],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::seconds(1)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 1024,
        stdin: Vec::new(),
        tool_name: Some("r3-soak-next".to_string()),
    })
    .await;

    match next {
        ManagedExecOutcome::Exited { stdout, .. } => assert_eq!(stdout, b"next"),
        other => panic!("expected next managed exec to run after timeout, got {other:?}"),
    }
}

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_cancellation_kills_process_group() {
    let token = CancellationToken::new();
    let child_token = token.clone();
    let handle = tokio::spawn(async move {
        run_managed_exec(ManagedExecRequest {
            argv: vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "sleep 5".to_string(),
            ],
            cwd: PathBuf::from("/"),
            deadline_at: None,
            cancellation_token: child_token,
            max_output_bytes: 1024,
            stdin: Vec::new(),
            tool_name: Some("test".to_string()),
        })
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !active_executor_snapshots().is_empty(),
        "executor should be visible while child is running"
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

#[cfg(unix)]
async fn wait_for_native_executor(tool_name: &str) -> crate::NativeExecutorStatus {
    let timeout_at = Instant::now() + Duration::from_millis(200);
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

#[cfg(unix)]
#[tokio::test]
async fn managed_exec_caps_stdout() {
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "printf 'abcdef'".to_string(),
        ],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::seconds(1)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 3,
        stdin: Vec::new(),
        tool_name: Some("test".to_string()),
    })
    .await;

    match outcome {
        ManagedExecOutcome::Exited {
            stdout,
            stdout_truncated,
            ..
        } => {
            assert_eq!(stdout, b"abc");
            assert!(stdout_truncated);
        }
        other => panic!("expected exited outcome, got {other:?}"),
    }
}
