use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::file_lock::FileLock;

use crate::support::process::ServeProcess;

const RETAINED_MARKER: &str = ".gents-failed-fixture-v1";
const MARKER_CONTENTS: &[u8] = b"completed failed fixture\n";
const DIRECTORY_PREFIX: &str = "failure-";
const MAX_RETAINED: usize = 5;

/// Retained homes contain accepted model inputs and credentials. Each run uses
/// a private temporary directory; only completed failures bearing this owner's
/// marker may be pruned. The server is stopped and reaped before that marker
/// is published, so concurrent active homes are never retention candidates.
pub struct FailureArtifacts {
    directory: Option<tempfile::TempDir>,
    server: Option<ServeProcess>,
    root: PathBuf,
    retain_failures: bool,
    succeeded: bool,
    limit: usize,
}

impl FailureArtifacts {
    pub fn new(root: &Path, retain_failures: bool) -> Result<Self> {
        let directory = if retain_failures {
            fs::create_dir_all(root)
                .with_context(|| format!("creating failure artifact root {}", root.display()))?;
            tempfile::Builder::new()
                .prefix(DIRECTORY_PREFIX)
                .tempdir_in(root)?
        } else {
            tempfile::tempdir()?
        };
        Ok(Self {
            directory: Some(directory),
            server: None,
            root: root.to_owned(),
            retain_failures,
            succeeded: false,
            limit: MAX_RETAINED,
        })
    }

    pub fn path(&self) -> &Path {
        self.directory.as_ref().expect("unfinished fixture").path()
    }

    pub fn attach_server(&mut self, server: ServeProcess) {
        assert!(self.server.is_none(), "fixture already has a server");
        self.server = Some(server);
    }

    pub fn captured_output(&self) -> Result<(String, String)> {
        self.server
            .as_ref()
            .context("fixture server was not started")?
            .captured_output()
    }

    pub fn finish<T>(mut self, result: Result<T>) -> Result<T> {
        self.succeeded = result.is_ok();
        match (result, self.finalize()) {
            (Err(error), Ok(Some(path))) => {
                Err(error.context(format!("failed fixture retained at {}", path.display())))
            }
            (Err(error), Err(retention)) => {
                Err(error.context(format!("finalizing failure artifacts: {retention:#}")))
            }
            (Ok(_), Err(error)) => Err(error),
            (result, _) => result,
        }
    }

    fn finalize(&mut self) -> Result<Option<PathBuf>> {
        drop(self.server.take());
        let Some(directory) = self.directory.take() else {
            return Ok(None);
        };
        if self.succeeded || !self.retain_failures {
            directory.close().context("cleaning fixture artifacts")?;
            return Ok(None);
        }

        let path = directory.keep();
        let retained = (|| {
            let lock = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.root.join(".retention.lock"))?;
            let _lock = FileLock::exclusive(lock)?;
            fs::write(path.join(RETAINED_MARKER), MARKER_CONTENTS)?;
            prune_completed_failures(&self.root, &path, self.limit)
        })();
        retained.with_context(|| format!("retaining failed fixture at {}", path.display()))?;
        tracing::error!(path = %path.display(), "retained failed fixture for investigation");
        Ok(Some(path))
    }
}

impl Drop for FailureArtifacts {
    fn drop(&mut self) {
        if let Err(error) = self.finalize() {
            tracing::error!(%error, "could not finalize fixture artifacts");
        }
    }
}

fn prune_completed_failures(root: &Path, current: &Path, limit: usize) -> Result<()> {
    let mut retained = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir()
            || !entry
                .file_name()
                .to_string_lossy()
                .starts_with(DIRECTORY_PREFIX)
        {
            continue;
        }
        let marker = entry.path().join(RETAINED_MARKER);
        let metadata = match marker.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if fs::read(&marker)? == MARKER_CONTENTS {
            retained.push((entry.path(), metadata.modified()?));
        }
    }
    retained.sort_by(|(left, left_time), (right, right_time)| {
        (left == current)
            .cmp(&(right == current))
            .then_with(|| left_time.cmp(right_time))
            .then_with(|| left.cmp(right))
    });
    let remove = retained.len().saturating_sub(limit.max(1));
    for (path, _) in retained.into_iter().take(remove) {
        fs::remove_dir_all(&path)
            .with_context(|| format!("pruning completed failure {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, retain: bool) -> FailureArtifacts {
        let fixture = FailureArtifacts::new(root, retain).unwrap();
        fs::create_dir_all(fixture.path().join("agent-home/data")).unwrap();
        fs::write(
            fixture.path().join("agent-home/data/evidence"),
            b"accepted input",
        )
        .unwrap();
        fixture
    }

    fn assert_evidence(path: &Path) {
        assert_eq!(
            fs::read(path.join("agent-home/data/evidence")).unwrap(),
            b"accepted input"
        );
    }

    #[test]
    fn success_cleans_up_and_failure_retains_the_home() {
        let root = tempfile::tempdir().unwrap();
        let success = fixture(root.path(), true);
        let success_path = success.path().to_owned();
        success.finish(Ok(())).unwrap();
        assert!(!success_path.exists());

        let failure = fixture(root.path(), true);
        let failure_path = failure.path().to_owned();
        let error = failure
            .finish::<()>(Err(anyhow::anyhow!("ledger mismatch")))
            .unwrap_err();
        assert!(format!("{error:#}").contains("ledger mismatch"));
        assert!(format!("{error:#}").contains(failure_path.to_str().unwrap()));
        assert_evidence(&failure_path);
        assert!(failure_path.join(RETAINED_MARKER).is_file());
    }

    #[test]
    fn early_return_and_panic_retain_evidence_without_finish() {
        let root = tempfile::tempdir().unwrap();
        let mut early_path = PathBuf::new();
        let result = (|| -> Result<()> {
            let guard = fixture(root.path(), true);
            early_path = guard.path().to_owned();
            anyhow::bail!("setup failed");
        })();
        assert!(result.is_err());
        assert_evidence(&early_path);

        let mut panic_path = PathBuf::new();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = fixture(root.path(), true);
            panic_path = guard.path().to_owned();
            panic!("ledger assertion failed");
        }));
        assert!(panic.is_err());
        assert_evidence(&panic_path);
        assert!(panic_path.join(RETAINED_MARKER).is_file());
    }

    #[test]
    fn opt_out_cleans_failed_and_panicking_fixtures() {
        let root = tempfile::tempdir().unwrap();
        let failure = fixture(root.path(), false);
        let path = failure.path().to_owned();
        assert!(failure
            .finish::<()>(Err(anyhow::anyhow!("failed")))
            .is_err());
        assert!(!path.exists());

        let mut path = PathBuf::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = fixture(root.path(), false);
            path = guard.path().to_owned();
            panic!("failed");
        }));
        assert!(!path.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn retention_is_bounded_without_pruning_active_or_unowned_directories() {
        let root = tempfile::tempdir().unwrap();
        let active = fixture(root.path(), true);
        let unrelated = root.path().join("failure-unrelated");
        fs::create_dir(&unrelated).unwrap();
        fs::write(unrelated.join(RETAINED_MARKER), b"someone else's fixture").unwrap();
        let mut paths = Vec::new();
        for _ in 0..4 {
            let mut failed = fixture(root.path(), true);
            failed.limit = 2;
            paths.push(failed.path().to_owned());
            assert!(failed.finish::<()>(Err(anyhow::anyhow!("failed"))).is_err());
        }
        assert_eq!(paths.iter().filter(|path| path.exists()).count(), 2);
        assert_evidence(paths.last().unwrap());
        assert_evidence(active.path());
        assert!(unrelated.is_dir());
        active.finish(Ok(())).unwrap();
    }

    #[test]
    fn concurrent_failures_share_the_retention_bound() {
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let mut failed = fixture(root.path(), true);
                failed.limit = 2;
                let path = failed.path().to_owned();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    assert!(failed.finish::<()>(Err(anyhow::anyhow!("failed"))).is_err());
                    path
                })
            })
            .collect::<Vec<_>>();
        let paths = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(paths.iter().filter(|path| path.exists()).count(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn pruning_does_not_follow_symlinked_directories_or_markers() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join(RETAINED_MARKER), MARKER_CONTENTS).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("failure-link")).unwrap();
        let linked_marker = root.path().join("failure-marker-link");
        fs::create_dir(&linked_marker).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join(RETAINED_MARKER),
            linked_marker.join(RETAINED_MARKER),
        )
        .unwrap();
        for _ in 0..2 {
            let mut failed = fixture(root.path(), true);
            failed.limit = 1;
            drop(failed);
        }
        assert!(outside.path().join(RETAINED_MARKER).exists());
        assert!(root
            .path()
            .join("failure-link")
            .symlink_metadata()
            .unwrap()
            .is_symlink());
        assert!(linked_marker.is_dir());
    }

    #[test]
    fn failed_server_startup_keeps_its_logs_with_the_home() {
        let root = tempfile::tempdir().unwrap();
        let failed = fixture(root.path(), true);
        let path = failed.path().to_owned();
        let result = crate::support::process::spawn_server_with_ready_json_in(
            &path.join("agent-home"),
            0,
            &["--invalid-fixture-argument"],
            &[],
            &path,
        );
        let error = match result {
            Ok(_) => panic!("invalid server arguments were accepted"),
            Err(error) => error,
        };
        assert!(failed.finish::<()>(Err(error)).is_err());
        assert_evidence(&path);
        let stderr = fs::read_dir(&path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("server.stderr.")
            })
            .unwrap();
        assert!(fs::read_to_string(stderr)
            .unwrap()
            .contains("invalid-fixture-argument"));
    }

    #[cfg(unix)]
    #[test]
    fn server_is_reaped_before_cleanup_or_retention_even_on_panic() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        for outcome in ["success", "error", "panic"] {
            let root = tempfile::tempdir().unwrap();
            let mut guard = fixture(root.path(), true);
            let path = guard.path().to_owned();
            let stdout = crate::support::process::server_log(Some(&path), "stdout").unwrap();
            let stderr = crate::support::process::server_log(Some(&path), "stderr").unwrap();
            let stdout_path = stdout.path().to_owned();
            let stderr_path = stderr.path().to_owned();
            let child = Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf accepted-input; printf diagnostic >&2; read release",
                ])
                .stdin(Stdio::piped())
                .stdout(stdout.reopen().unwrap())
                .stderr(stderr.reopen().unwrap())
                .spawn()
                .unwrap();
            let pid = child.id();
            guard.attach_server(ServeProcess::with_logs(child, stdout, stderr));
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let (stdout, stderr) = guard.captured_output().unwrap();
                if stdout == "accepted-input" && stderr == "diagnostic" {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "server did not write its evidence"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            match outcome {
                "success" => guard.finish(Ok(())).unwrap(),
                "error" => {
                    assert!(guard.finish::<()>(Err(anyhow::anyhow!("failed"))).is_err());
                }
                "panic" => {
                    let panicked =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                            let _guard = guard;
                            panic!("ledger assertion");
                        }));
                    assert!(panicked.is_err());
                }
                _ => unreachable!(),
            }
            let mut status = 0;
            assert_eq!(
                // SAFETY: status is writable, and pid identifies this test's child.
                unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            if outcome == "success" {
                assert!(!path.exists());
            } else {
                assert_evidence(&path);
                assert_eq!(fs::read_to_string(stdout_path).unwrap(), "accepted-input");
                assert_eq!(fs::read_to_string(stderr_path).unwrap(), "diagnostic");
            }
        }
    }
}
