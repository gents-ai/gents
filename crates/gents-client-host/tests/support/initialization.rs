use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use tokio::process::{Child, Command};

/// Descendants can inherit output handles after the CLI has exited. File capture
/// keeps initialization completion independent of pipe EOF and retains output
/// when the direct process must be terminated.
pub struct Initialization {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
    started: Instant,
}

impl Initialization {
    pub fn spawn(command: &mut Command, root: &Path) -> Result<Self> {
        let stdout = root.join("init.stdout");
        let stderr = root.join("init.stderr");
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&stdout)?))
            .stderr(Stdio::from(std::fs::File::create(&stderr)?))
            .kill_on_drop(true)
            .spawn()
            .context("starting fixture initialization")?;
        Ok(Self {
            child,
            stdout,
            stderr,
            started: Instant::now(),
        })
    }

    pub async fn wait(mut self, deadline: Duration) -> Result<Vec<u8>> {
        let result = async {
            let status = match tokio::time::timeout(deadline, self.child.wait()).await {
                Ok(status) => status.context("waiting for fixture initialization")?,
                Err(error) => {
                    self.child
                        .kill()
                        .await
                        .context("terminating timed-out fixture initialization")?;
                    return Err(error).context("initializing the fixture runtime timed out");
                }
            };
            ensure!(
                status.success(),
                "fixture initialization exited with {status}"
            );
            std::fs::read(&self.stdout).context("reading fixture initialization output")
        }
        .await;
        result.with_context(|| {
            format!(
                "fixture initialization after {:.3}s\ninit.stdout:\n{}\ninit.stderr:\n{}",
                self.started.elapsed().as_secs_f64(),
                diagnostic(&self.stdout),
                diagnostic(&self.stderr),
            )
        })
    }
}

fn diagnostic(path: &Path) -> String {
    use std::io::Read;
    let mut bytes = Vec::new();
    match std::fs::File::open(path).and_then(|file| file.take(65_536).read_to_end(&mut bytes)) {
        Ok(_) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(error) => format!("could not read {}: {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const ROLE: &str = "GENTS_INITIALIZATION_PROBE_ROLE";
    const ROOT: &str = "GENTS_INITIALIZATION_PROBE_ROOT";

    fn command(root: &Path, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "initialization::tests::probe_child",
                "--nocapture",
            ])
            .env(ROLE, role)
            .env(ROOT, root);
        command
    }

    struct Release(PathBuf);

    impl Drop for Release {
        fn drop(&mut self) {
            let _ = std::fs::write(self.0.join("release"), []);
        }
    }

    async fn ready(root: &Path) {
        tokio::time::timeout(Duration::from_secs(30), async {
            while !root.join("ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initialization probe did not become ready");
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous probe process waiting on cross-process marker files"
    )]
    fn probe_child() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
        if role == "parent" {
            let mut holder = command(&root, "holder");
            #[expect(
                clippy::zombie_processes,
                reason = "the probe parent must exit while its descendant retains output handles; the harness releases that descendant through a marker"
            )]
            let _child = holder
                .as_std_mut()
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            while !root.join("ready").is_file() {
                assert!(Instant::now() < deadline, "output holder did not start");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if role != "holder" {
            std::io::stdout()
                .write_all(b"initialization stdout marker\n")
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr()
                .write_all(b"initialization stderr marker\n")
                .unwrap();
            std::io::stderr().flush().unwrap();
        }
        if role == "failure" {
            panic!("initialization failure marker");
        }
        if role == "parent" {
            return;
        }
        std::fs::write(root.join("ready"), []).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while root.exists() && !root.join("release").is_file() {
            assert!(
                Instant::now() < deadline,
                "initialization probe was not released"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[tokio::test]
    async fn initialization_finishes_while_a_descendant_keeps_output_handles_open() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let init = Initialization::spawn(&mut command(root.path(), "parent"), root.path()).unwrap();
        let output = init.wait(Duration::from_secs(30)).await.unwrap();
        assert!(String::from_utf8_lossy(&output).contains("initialization stdout marker"));
        assert!(root.path().join("ready").is_file());
        assert!(!root.path().join("release").exists());
    }

    #[tokio::test]
    async fn initialization_timeout_retains_partial_output_and_reaps_the_process() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let init =
            Initialization::spawn(&mut command(root.path(), "waiting"), root.path()).unwrap();
        ready(root.path()).await;
        let error = init.wait(Duration::ZERO).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("initializing the fixture runtime timed out"));
        assert!(error.contains("initialization stdout marker"));
        assert!(error.contains("initialization stderr marker"));
    }

    #[tokio::test]
    async fn initialization_failure_retains_both_output_streams() {
        let root = tempfile::tempdir().unwrap();
        let init =
            Initialization::spawn(&mut command(root.path(), "failure"), root.path()).unwrap();
        let error = init.wait(Duration::from_secs(30)).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("fixture initialization exited with"));
        assert!(error.contains("initialization stdout marker"));
        assert!(error.contains("initialization stderr marker"));
        assert!(error.contains("initialization failure marker"));
    }
}
