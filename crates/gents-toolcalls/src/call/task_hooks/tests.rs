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
    let directory = tempfile::tempdir().expect("hook marker directory");
    let started = directory.path().join("started");
    let finished = directory.path().join("finished");
    assert!(tokio::process::Command::new("mkfifo")
        .arg(&started)
        .status()
        .await
        .unwrap()
        .success());
    let cancellation = TaskHookCancellation::default();
    let exec = ManagedTaskHookExec::new(directory.path().to_path_buf(), cancellation.clone());
    let script = format!(
        ": > {}; sleep 30; touch {}",
        started.display(),
        finished.display()
    );
    let interrupt = async {
        let mut pipe = tokio::net::unix::pipe::OpenOptions::new()
            .open_receiver(&started)
            .unwrap();
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buf)
            .await
            .unwrap();
        cancellation.interrupt();
    };
    let held = hook("held", &["sh", "-c", &script], Some(60));
    let (attempt, ()) = tokio::join!(exec.attempt(&held), interrupt);
    assert_eq!(attempt.result, HookCommandResult::Interrupted);
    assert!(!finished.exists());
}
