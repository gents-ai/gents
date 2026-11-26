use super::{HookCommandResult, ManagedTaskHookExec, TaskHookCancellation, TaskHookExec};
use gents_documents::{TaskHook, TaskHookPhase};

fn hook(hook_id: &str, command: &[&str], timeout_secs: Option<i64>) -> TaskHook {
    phased(hook_id, TaskHookPhase::Before, command, timeout_secs)
}

fn phased(
    hook_id: &str,
    phase: TaskHookPhase,
    command: &[&str],
    timeout_secs: Option<i64>,
) -> TaskHook {
    TaskHook {
        hook_id: hook_id.to_string(),
        phase,
        command: command.iter().map(|part| (*part).to_string()).collect(),
        timeout_secs,
        skip_exit_codes: Vec::new(),
    }
}

#[tokio::test]
async fn an_interrupt_cancels_ordinary_phases_before_launch_but_not_cleanup() {
    let directory = tempfile::tempdir().expect("hook marker directory");
    let marker = |name: &str| directory.path().join(name);
    let touch = |name: &str| format!("touch {}", marker(name).display());
    let cancellation = TaskHookCancellation::default();
    cancellation.interrupt();
    let exec = ManagedTaskHookExec::new(directory.path().to_path_buf(), cancellation.clone());

    let before = exec
        .attempt(&hook("prepare", &["sh", "-c", &touch("prepare")], Some(30)))
        .await;
    assert_eq!(before.result, HookCommandResult::Interrupted);
    assert!(
        !marker("prepare").exists(),
        "a cancelled hook never launches"
    );

    let cleanup = exec
        .attempt(&phased(
            "sweep",
            TaskHookPhase::Finally,
            &["sh", "-c", &touch("sweep")],
            Some(30),
        ))
        .await;
    assert_eq!(cleanup.result, HookCommandResult::Exited { code: Some(0) });
    assert!(
        marker("sweep").exists(),
        "an interrupt never cancels cleanup"
    );

    cancellation.shutdown();
    let after_shutdown = exec
        .attempt(&phased(
            "late",
            TaskHookPhase::Finally,
            &["sh", "-c", &touch("late")],
            Some(30),
        ))
        .await;
    assert_eq!(after_shutdown.result, HookCommandResult::Interrupted);
    assert!(!marker("late").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn an_interrupt_stops_a_running_hook() {
    use crate::test_support::held_process::{HeldProcess, HOLD};
    use gents_store::test_support::UNREACHED_DEADLINE;

    let directory = tempfile::tempdir().expect("hook marker directory");
    let mut process = HeldProcess::new(directory.path()).await;
    let cancellation = TaskHookCancellation::default();
    let exec = ManagedTaskHookExec::new(directory.path().to_path_buf(), cancellation.clone());
    let script = format!("{HOLD}; touch finished");
    let held = hook(
        "held",
        &["sh", "-c", &script],
        Some(i64::try_from(UNREACHED_DEADLINE.as_secs()).unwrap()),
    );
    let interrupt = async {
        process.entered().await;
        cancellation.interrupt();
    };
    let (attempt, ()) = tokio::join!(exec.attempt(&held), interrupt);
    assert_eq!(attempt.result, HookCommandResult::Interrupted);
    assert!(!directory.path().join("finished").exists());
}
