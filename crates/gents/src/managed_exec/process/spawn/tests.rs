use std::collections::HashMap;
use std::os::unix::fs::{symlink, PermissionsExt};
#[cfg(target_os = "macos")]
use std::path::Path;
use std::path::PathBuf;

use tokio_util::sync::CancellationToken;

use super::super::{run_managed_exec, ManagedExecOutcome, ManagedExecRequest};

async fn capture(cwd: PathBuf, path: &str, program: &str, script: &str) -> ManagedExecOutcome {
    run_managed_exec(ManagedExecRequest {
        argv: vec![program.into(), "-c".into(), script.into()],
        cwd,
        deadline_at: None,
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 1024,
        stdin: Vec::new(),
        environment: Some(HashMap::from([("PATH".into(), path.into())])),
        tool_name: None,
        live_output: None,
    })
    .await
}

fn complete_output(outcome: ManagedExecOutcome) -> Vec<u8> {
    let ManagedExecOutcome::Exited {
        code,
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
    } = outcome
    else {
        panic!("expected a successful exit, got {outcome:?}");
    };
    assert_eq!(
        code,
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert!(!stdout_truncated, "stdout capture is incomplete");
    assert!(!stderr_truncated, "stderr capture is incomplete");
    stdout
}

#[tokio::test]
async fn supplied_path_preserves_relative_cwd_argv_zero_and_environment() {
    let directory = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bin = directory.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    symlink("/bin/sh", bin.join("fixture-shell")).unwrap();
    let stdout = complete_output(
        capture(
            PathBuf::from(directory.path().file_name().unwrap()),
            "bin",
            "fixture-shell",
            "printf '%s|%s|%s' \"$0\" \"$PATH\" \"${HOME-unset}\"",
        )
        .await,
    );
    assert_eq!(stdout, b"fixture-shell|bin|unset");
}

#[tokio::test]
async fn supplied_path_retains_native_permission_search() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["first", "second"] {
        std::fs::create_dir(directory.path().join(name)).unwrap();
    }
    let denied = directory.path().join("first/fixture-shell");
    std::fs::write(&denied, "not executable").unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o644)).unwrap();
    symlink("/bin/sh", directory.path().join("second/fixture-shell")).unwrap();
    assert_eq!(
        complete_output(
            capture(
                directory.path().to_path_buf(),
                "first:second",
                "fixture-shell",
                "printf found",
            )
            .await,
        ),
        b"found"
    );
}

#[tokio::test]
async fn supplied_path_retains_native_script_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("fixture-command");
    std::fs::write(&script, "printf fallback").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        complete_output(
            capture(
                directory.path().to_path_buf(),
                ".",
                "fixture-command",
                "unused",
            )
            .await,
        ),
        b"fallback"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn concurrent_spawns_release_unrelated_output_writers() {
    const FIXTURE: &str = "GENTS_CAPTURE_SPAWN_FIXTURE";
    if let Some(root) = std::env::var_os(FIXTURE) {
        exercise_concurrent_spawns(Path::new(&root));
        std::fs::write(Path::new(&root).join("verified"), b"complete").unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fork_delay.c");
    let library = root.path().join("fork_delay.dylib");
    std::fs::write(&source, include_str!("fork_delay.c")).unwrap();
    let compiler = std::process::Command::new("/usr/bin/cc")
        .args(["-dynamiclib", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let mut environment = std::env::vars().collect::<HashMap<_, _>>();
    environment.insert(FIXTURE.into(), root.path().to_str().unwrap().into());
    environment.insert(
        "DYLD_INSERT_LIBRARIES".into(),
        library.to_str().unwrap().into(),
    );
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: vec![
            std::env::current_exe().unwrap().to_str().unwrap().into(),
            "--exact".into(),
            "managed_exec::process::spawn::tests::concurrent_spawns_release_unrelated_output_writers"
                .into(),
            "--nocapture".into(),
        ],
        cwd: root.path().to_path_buf(),
        deadline_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
        cancellation_token: CancellationToken::new(),
        max_output_bytes: 64 * 1024,
        stdin: Vec::new(),
        environment: Some(environment),
        tool_name: None,
        live_output: None,
    })
    .await;
    complete_output(outcome);
    assert!(root.path().join("verified").is_file());
}

#[cfg(target_os = "macos")]
fn exercise_concurrent_spawns(root: &Path) {
    use super::super::{spawn_managed_process, ManagedExecKind, SpawnManagedProcessRequest};
    use std::time::{Duration, Instant};

    let first = std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(capture(
                PathBuf::from("/"),
                "/usr/bin:/bin",
                "sh",
                "printf finite",
            ))
    });
    let start = Instant::now();
    while !root.join("first").exists() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(1));
    }
    let second = std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut child = spawn_managed_process(SpawnManagedProcessRequest {
                    argv: vec!["sh".into(), "-c".into(), ":".into()],
                    cwd: PathBuf::from("/"),
                    environment: Some(HashMap::from([("PATH".into(), "/usr/bin:/bin".into())])),
                    tool_name: None,
                    kind: ManagedExecKind::PersistentService,
                })
                .await
                .unwrap();
                child.terminate().await;
            });
    });
    let outcome = first.join().unwrap();
    std::fs::write(root.join("release"), b"capture finished").unwrap();
    second.join().unwrap();
    assert_eq!(complete_output(outcome), b"finite");
}
