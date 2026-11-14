use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use anyhow::{anyhow, ensure, Context, Result};
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

/// Output length is sampled on a timer, so an observed instant is the first
/// sample that saw the write, not the write itself; it separates seconds, not
/// milliseconds.
#[derive(Default)]
struct Output {
    length: u64,
    first: Option<Instant>,
    settled: Option<Instant>,
}

const SAMPLE: Duration = Duration::from_millis(100);
const CAPTURE: u64 = 65_536;

enum Keep {
    Head,
    Tail,
}

impl Output {
    async fn sample(&mut self, path: &Path) {
        let Ok(length) = tokio::fs::metadata(path)
            .await
            .map(|metadata| metadata.len())
        else {
            return;
        };
        if length == 0 {
            return;
        }
        let now = Instant::now();
        if self.first.is_none() {
            self.first = Some(now);
        }
        if length != self.length {
            self.length = length;
            self.settled = Some(now);
        }
    }
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
        let mut output = Output::default();
        let mut exit = None;
        let mut termination = None;
        let result = async {
            match self.sample_until_exit(deadline, &mut output).await {
                Ok(status) => {
                    exit = Some(self.started.elapsed());
                    let status = status.context("waiting for fixture initialization")?;
                    ensure!(
                        status.success(),
                        "fixture initialization exited with {status}"
                    );
                    tokio::fs::read(&self.stdout)
                        .await
                        .context("reading fixture initialization output")
                }
                Err(()) => {
                    termination = Some(self.terminate().await);
                    Err(anyhow!("initializing the fixture runtime timed out"))
                }
            }
        }
        .await;
        match result {
            Ok(bytes) => Ok(bytes),
            Err(error) => {
                let elapsed = self.started.elapsed();
                let diagnostic = tokio::task::spawn_blocking(move || {
                    format!(
                        "fixture initialization after {:.3}s\n{}\ninit.stdout:\n{}\ninit.stderr:\n{}",
                        elapsed.as_secs_f64(),
                        self.verdict(&output, exit, termination.as_deref(), elapsed),
                        capture(&self.stdout, Keep::Head),
                        capture(&self.stderr, Keep::Tail),
                    )
                })
                .await
                .context("collecting fixture initialization diagnostics")?;
                Err(error.context(diagnostic))
            }
        }
    }

    async fn sample_until_exit(
        &mut self,
        deadline: Duration,
        output: &mut Output,
    ) -> Result<std::io::Result<std::process::ExitStatus>, ()> {
        let until = tokio::time::Instant::now() + deadline;
        output.sample(&self.stdout).await;
        let status = tokio::time::timeout_at(until, async {
            loop {
                tokio::select! {
                    biased;
                    status = self.child.wait() => return status,
                    () = tokio::time::sleep(SAMPLE) => output.sample(&self.stdout).await,
                }
            }
        })
        .await;
        output.sample(&self.stdout).await;
        status.map_err(|_| ())
    }

    async fn terminate(&mut self) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("had already exited with {status}"),
            Ok(None) => match self.child.kill().await {
                Ok(()) => "killed".to_owned(),
                Err(error) => format!("could not be killed: {error}"),
            },
            Err(error) => format!("exit status unavailable: {error}"),
        }
    }

    fn verdict(
        &self,
        output: &Output,
        exit: Option<Duration>,
        termination: Option<&str>,
        elapsed: Duration,
    ) -> String {
        let stdout = match (output.first, output.settled) {
            (Some(_), Some(settled)) => format!(
                "stdout complete at {:.3}s ({})",
                settled.duration_since(self.started).as_secs_f64(),
                self.reported_status()
            ),
            _ => format!("no stdout at {:.3}s", elapsed.as_secs_f64()),
        };
        let process = match (exit, termination) {
            (Some(exit), _) => format!("process exited at {:.3}s", exit.as_secs_f64()),
            (None, termination) => format!(
                "process still running at {:.3}s ({})",
                elapsed.as_secs_f64(),
                termination.unwrap_or("not terminated")
            ),
        };
        format!("verdict: {stdout}; {process}")
    }

    fn reported_status(&self) -> String {
        let Ok(bytes) = std::fs::read(&self.stdout) else {
            return "stdout unreadable".to_owned();
        };
        match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => match value.get("status").and_then(serde_json::Value::as_str) {
                Some(status) => format!("status={status}"),
                None => "JSON without a status field".to_owned(),
            },
            Err(_) => "stdout is not JSON".to_owned(),
        }
    }
}

fn capture(path: &Path, keep: Keep) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return format!("could not read {}: {error}", path.display()),
    };
    let length = match file.metadata() {
        Ok(metadata) => metadata.len(),
        Err(error) => return format!("could not read {}: {error}", path.display()),
    };
    let dropped = length.saturating_sub(CAPTURE);
    if dropped > 0 && matches!(keep, Keep::Tail) {
        if let Err(error) = file.seek(SeekFrom::Start(dropped)) {
            return format!("could not read {}: {error}", path.display());
        }
    }
    let mut bytes = Vec::new();
    if let Err(error) = file.take(CAPTURE).read_to_end(&mut bytes) {
        return format!("could not read {}: {error}", path.display());
    }
    let text = String::from_utf8_lossy(&bytes);
    if dropped == 0 {
        return text.into_owned();
    }
    let notice = format!("<truncated, {dropped} of {length} bytes dropped>");
    match keep {
        Keep::Head => format!("{text}\n{notice} (tail dropped)"),
        Keep::Tail => format!("{notice} (head dropped)\n{text}"),
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
                "runtime::initialization::tests::probe_child",
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
        // libtest writes its own progress banner to the inherited stdout, so a
        // probe that must stand in for a CLI producing no output clears the
        // capture before it signals readiness.
        if matches!(role.as_str(), "silent" | "flooding") {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(root.join("init.stdout"))
                .unwrap();
        }
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
        if role == "flooding" {
            let mut flood = vec![b'f'; 70_000];
            flood.extend_from_slice(b"\ninitialization stderr tail marker\n");
            std::io::stderr().write_all(&flood).unwrap();
            std::io::stderr().flush().unwrap();
        }
        if !matches!(role.as_str(), "holder" | "silent" | "flooding") {
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

    #[tokio::test]
    async fn initialization_timeout_names_the_output_and_the_exit_phase() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let init =
            Initialization::spawn(&mut command(root.path(), "waiting"), root.path()).unwrap();
        ready(root.path()).await;
        let error = init.wait(Duration::from_millis(250)).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("stdout complete at"), "{error}");
        assert!(error.contains("stdout is not JSON"), "{error}");
        assert!(error.contains("process still running at"), "{error}");
        assert!(error.contains("killed"), "{error}");
    }

    #[tokio::test]
    async fn initialization_timeout_separates_absent_output_from_complete_output() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let init = Initialization::spawn(&mut command(root.path(), "silent"), root.path()).unwrap();
        ready(root.path()).await;
        let error = init.wait(Duration::from_millis(250)).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("no stdout at"), "{error}");
        assert!(!error.contains("stdout complete at"), "{error}");
        assert!(error.contains("process still running at"), "{error}");
    }

    #[tokio::test]
    async fn initialization_timeout_announces_truncation_and_keeps_the_stderr_tail() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let init =
            Initialization::spawn(&mut command(root.path(), "flooding"), root.path()).unwrap();
        ready(root.path()).await;
        let error = init.wait(Duration::from_millis(250)).await.unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("initialization stderr tail marker"),
            "{error}"
        );
        assert!(error.contains("bytes dropped"), "{error}");
    }
}
