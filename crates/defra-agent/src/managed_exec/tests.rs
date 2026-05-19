use std::path::PathBuf;
use std::time::Duration;

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
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo started; trap '' TERM; sleep 5".to_string(),
        ],
        cwd: PathBuf::from("/"),
        deadline_at: Some(Utc::now() + chrono::Duration::milliseconds(30)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 1024,
        stdin: Vec::new(),
        tool_name: Some("test".to_string()),
    })
    .await;

    match outcome {
        ManagedExecOutcome::TimedOut { stdout, kill, .. } => {
            assert!(String::from_utf8_lossy(&stdout).contains("started"));
            assert!(kill.term_signal_sent);
            assert!(kill.kill_signal_sent);
            assert!(kill.reaped);
        }
        other => panic!("expected timeout outcome, got {other:?}"),
    }
    assert!(active_executor_snapshots().is_empty());
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
