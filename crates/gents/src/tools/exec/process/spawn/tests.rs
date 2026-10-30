use rapidhash::RapidHashMap;
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
        environment: Some(RapidHashMap::from_iter([("PATH".into(), path.into())])),
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
    #[cfg(target_os = "macos")]
    {
        const FIXTURE: &str = "GENTS_SUPPLIED_PATH_RELATIVE_FIXTURE";
        if let Some(root) = std::env::var_os(FIXTURE) {
            exercise_supplied_path_preserves_relative_cwd_argv_zero_and_environment().await;
            tokio::fs::write(PathBuf::from(root).join("verified"), b"complete")
                .await
                .unwrap();
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let mut environment = std::env::vars().collect::<RapidHashMap<_, _>>();
        environment.insert(FIXTURE.into(), root.path().to_string_lossy().into_owned());
        run_isolated_capture_test(
            "tools::exec::process::spawn::tests::supplied_path_preserves_relative_cwd_argv_zero_and_environment",
            root.path(),
            environment,
        )
        .await;
    }
    #[cfg(not(target_os = "macos"))]
    exercise_supplied_path_preserves_relative_cwd_argv_zero_and_environment().await;
}

async fn exercise_supplied_path_preserves_relative_cwd_argv_zero_and_environment() {
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
async fn supplied_path_preserves_native_spawn_errors() {
    let root = tempfile::tempdir().unwrap();
    let denied = root.path().join("denied");
    std::fs::create_dir(&denied).unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o600)).unwrap();
    for (program, cwd, path) in [
        ("missing", root.path().to_path_buf(), "."),
        ("sh", root.path().join("missing"), "/bin"),
        ("sh", PathBuf::new(), "/bin"),
        ("sh", denied.clone(), "/bin"),
        ("sh\0", root.path().to_path_buf(), "/bin"),
    ] {
        use std::os::unix::process::CommandExt;
        let mut reference = tokio::process::Command::new(program);
        reference.current_dir(&cwd).env_clear().env("PATH", path);
        reference.as_std_mut().arg0(program);
        let expected = reference.spawn().expect_err("invalid native spawn").kind();
        let environment = RapidHashMap::from_iter([("PATH".into(), path.into())]);
        let actual = super::spawn_child(program, &[], &cwd, Some(&environment), false)
            .expect_err("invalid managed spawn")
            .kind();
        assert_eq!(actual, expected, "program {program:?}, cwd {cwd:?}");
    }
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    concurrent_spawn_fixture(false).await;
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn permission_search_releases_unrelated_output_writers() {
    concurrent_spawn_fixture(true).await;
}

#[cfg(target_os = "macos")]
async fn concurrent_spawn_fixture(permission_search: bool) {
    const FIXTURE: &str = "GENTS_CAPTURE_SPAWN_FIXTURE";
    if let Some(root) = std::env::var_os(FIXTURE) {
        exercise_concurrent_spawns(Path::new(&root), permission_search);
        std::fs::write(Path::new(&root).join("verified"), b"complete").unwrap();
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("fork_delay.c");
    let library = root.path().join("fork_delay.dylib");
    std::fs::write(&source, include_str!("fork_delay.c")).unwrap();
    let compiler = tokio::process::Command::new("/usr/bin/cc")
        .args(["-dynamiclib", "-o"])
        .arg(&library)
        .arg(&source)
        .output()
        .await
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    let mut environment = std::env::vars().collect::<RapidHashMap<_, _>>();
    environment.insert(FIXTURE.into(), root.path().to_str().unwrap().into());
    environment.insert(
        "DYLD_INSERT_LIBRARIES".into(),
        library.to_str().unwrap().into(),
    );
    run_isolated_capture_test(
        if permission_search {
            "tools::exec::process::spawn::tests::permission_search_releases_unrelated_output_writers"
        } else {
            "tools::exec::process::spawn::tests::concurrent_spawns_release_unrelated_output_writers"
        },
        root.path(),
        environment,
    )
    .await;
}

#[cfg(target_os = "macos")]
async fn run_isolated_capture_test(
    test_name: &str,
    root: &Path,
    environment: RapidHashMap<String, String>,
) {
    let stdout_path = root.join("probe.stdout");
    let stderr_path = root.join("probe.stderr");
    let stdout = tokio::fs::File::create(&stdout_path)
        .await
        .unwrap()
        .into_std()
        .await;
    let stderr = tokio::fs::File::create(&stderr_path)
        .await
        .unwrap()
        .into_std()
        .await;
    let mut child = super::super::ManagedChild::new(
        tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .current_dir(root)
            .env_clear()
            .envs(environment)
            .stdin(std::process::Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .kill_on_drop(true)
            .process_group(0)
            .spawn()
            .unwrap(),
    );
    let status = tokio::time::timeout(std::time::Duration::from_secs(30), child.inner.wait()).await;
    if matches!(&status, Ok(Ok(_))) {
        child.mark_finished(true);
    } else {
        child.terminate_service().await;
    }
    let stdout = probe_output(&stdout_path).await;
    let stderr = probe_output(&stderr_path).await;
    assert!(
        matches!(&status, Ok(Ok(status)) if status.success()),
        "probe status: {status:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        tokio::fs::read(root.join("verified")).await.unwrap(),
        b"complete"
    );
}

/// Other tests' native reference forks can retain the driver's pipe writers.
/// File capture isolates that driver; the probe itself still checks managed
/// stdout/stderr completeness before publishing its verified marker.
#[cfg(target_os = "macos")]
async fn probe_output(path: &Path) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await
        .unwrap()
        .take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .unwrap();
    assert!(
        bytes.len() <= 64 * 1024,
        "isolated spawn probe output exceeded its bound"
    );
    bytes
}

#[cfg(target_os = "macos")]
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous test waiting for a child process marker file"
)]
fn exercise_concurrent_spawns(root: &Path, permission_search: bool) {
    use super::super::{spawn_managed_process, ManagedExecKind, SpawnManagedProcessRequest};
    use std::time::{Duration, Instant};

    let search_path = if permission_search {
        let denied = root.join("denied");
        std::fs::create_dir(&denied).unwrap();
        std::fs::write(denied.join("sh"), b"not executable").unwrap();
        std::fs::set_permissions(denied.join("sh"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        format!("{}:/usr/bin:/bin", denied.display())
    } else {
        "/usr/bin:/bin".to_string()
    };
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
    let second = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut child = spawn_managed_process(SpawnManagedProcessRequest {
                    argv: vec!["sh".into(), "-c".into(), ":".into()],
                    cwd: PathBuf::from("/"),
                    environment: Some(RapidHashMap::from_iter([("PATH".into(), search_path)])),
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
