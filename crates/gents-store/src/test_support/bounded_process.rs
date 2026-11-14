use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

/// A spawned command with one deadline, its output captured to files.
///
/// Descendants can inherit output handles after the command has exited. File
/// capture keeps completion independent of pipe EOF and retains output when
/// the direct process must be terminated. Waiting samples the child from the
/// calling thread, so an asynchronous caller runs it on `spawn_blocking`.
pub struct BoundedProcess {
    child: Child,
    what: String,
    stdout: PathBuf,
    stderr: PathBuf,
    started: Instant,
}

pub struct Exited {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
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
    fn sample(&mut self, path: &Path) {
        let Ok(length) = std::fs::metadata(path).map(|metadata| metadata.len()) else {
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

impl Drop for BoundedProcess {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

impl BoundedProcess {
    /// Captures to `<stem>.stdout` and `<stem>.stderr` under `capture`; `what`
    /// names the command in every failure.
    pub fn spawn(command: &mut Command, capture: &Path, stem: &str, what: &str) -> Result<Self> {
        let stdout = capture.join(format!("{stem}.stdout"));
        let stderr = capture.join(format!("{stem}.stderr"));
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(std::fs::File::create(&stdout)?))
            .stderr(Stdio::from(std::fs::File::create(&stderr)?))
            .spawn()
            .with_context(|| format!("starting {what}"))?;
        Ok(Self {
            child,
            what: what.to_owned(),
            stdout,
            stderr,
            started: Instant::now(),
        })
    }

    /// The captured stdout of a command that exited successfully in time.
    pub fn wait(self, deadline: Duration) -> Result<Vec<u8>> {
        let what = self.what.clone();
        let (stdout, stderr) = (self.stdout.clone(), self.stderr.clone());
        let started = self.started;
        let exited = self.exit(deadline)?;
        if exited.status.success() {
            return Ok(exited.stdout);
        }
        let elapsed = started.elapsed();
        Err(
            anyhow!("{what} exited with {}", exited.status).context(format!(
                "{what} after {:.3}s\nverdict: process exited at {:.3}s\n{}",
                elapsed.as_secs_f64(),
                elapsed.as_secs_f64(),
                captured(&stdout, &stderr),
            )),
        )
    }

    /// The exit status and captured output of a command that exited in time,
    /// whatever its status. A command still running at the deadline is killed
    /// and reported with a verdict that separates output it never produced
    /// from output it completed before hanging.
    pub fn exit(mut self, deadline: Duration) -> Result<Exited> {
        let mut output = Output::default();
        match self.sample_until_exit(deadline, &mut output) {
            Some(status) => {
                let status = status.with_context(|| format!("waiting for {}", self.what))?;
                Ok(Exited {
                    status,
                    stdout: std::fs::read(&self.stdout)
                        .with_context(|| format!("reading {} output", self.what))?,
                    stderr: std::fs::read(&self.stderr)
                        .with_context(|| format!("reading {} errors", self.what))?,
                })
            }
            None => {
                let termination = self.terminate();
                let elapsed = self.started.elapsed();
                Err(anyhow!("{} timed out", self.what).context(format!(
                    "{} after {:.3}s\n{}\n{}",
                    self.what,
                    elapsed.as_secs_f64(),
                    self.verdict(&output, &termination, elapsed),
                    captured(&self.stdout, &self.stderr),
                )))
            }
        }
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous harness sampling a std child process and its capture file"
    )]
    fn sample_until_exit(
        &mut self,
        deadline: Duration,
        output: &mut Output,
    ) -> Option<std::io::Result<ExitStatus>> {
        let until = Instant::now() + deadline;
        loop {
            output.sample(&self.stdout);
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    output.sample(&self.stdout);
                    return Some(Ok(status));
                }
                Ok(None) => {}
                Err(error) => return Some(Err(error)),
            }
            let now = Instant::now();
            if now >= until {
                return None;
            }
            std::thread::sleep(SAMPLE.min(until - now));
        }
    }

    fn terminate(&mut self) -> String {
        match self.child.try_wait() {
            Ok(Some(status)) => format!("had already exited with {status}"),
            Ok(None) => match self.child.kill().and_then(|()| self.child.wait()) {
                Ok(_) => "killed".to_owned(),
                Err(error) => format!("could not be killed: {error}"),
            },
            Err(error) => format!("exit status unavailable: {error}"),
        }
    }

    fn verdict(&self, output: &Output, termination: &str, elapsed: Duration) -> String {
        let stdout = match (output.first, output.settled) {
            (Some(_), Some(settled)) => format!(
                "stdout complete at {:.3}s ({})",
                settled.duration_since(self.started).as_secs_f64(),
                self.reported_status()
            ),
            _ => format!("no stdout at {:.3}s", elapsed.as_secs_f64()),
        };
        format!(
            "verdict: {stdout}; process still running at {:.3}s ({termination})",
            elapsed.as_secs_f64()
        )
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

fn captured(stdout: &Path, stderr: &Path) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        capture(stdout, Keep::Head),
        capture(stderr, Keep::Tail)
    )
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

    const ROLE: &str = "GENTS_BOUNDED_PROCESS_PROBE_ROLE";
    const ROOT: &str = "GENTS_BOUNDED_PROCESS_PROBE_ROOT";
    const WHAT: &str = "the probe";

    fn spawn(root: &Path, role: &str) -> BoundedProcess {
        BoundedProcess::spawn(&mut command(root, role), root, "probe", WHAT).unwrap()
    }

    fn command(root: &Path, role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "test_support::bounded_process::tests::probe_child",
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

    #[expect(
        clippy::disallowed_methods,
        reason = "synchronous test waiting on a cross-process marker file"
    )]
    fn ready(root: &Path) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !root.join("ready").is_file() {
            assert!(Instant::now() < deadline, "probe did not become ready");
            std::thread::sleep(Duration::from_millis(10));
        }
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
        // probe that must stand in for a command producing no output clears
        // the capture before it signals readiness.
        if matches!(role.as_str(), "silent" | "flooding") {
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(root.join("probe.stdout"))
                .unwrap();
        }
        if role == "parent" {
            #[expect(
                clippy::zombie_processes,
                reason = "the probe parent must exit while its descendant retains output handles; the harness releases that descendant through a marker"
            )]
            let _child = command(&root, "holder")
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
            flood.extend_from_slice(b"\nprobe stderr tail marker\n");
            std::io::stderr().write_all(&flood).unwrap();
            std::io::stderr().flush().unwrap();
        }
        if !matches!(role.as_str(), "holder" | "silent" | "flooding") {
            std::io::stdout()
                .write_all(b"probe stdout marker\n")
                .unwrap();
            std::io::stdout().flush().unwrap();
            std::io::stderr()
                .write_all(b"probe stderr marker\n")
                .unwrap();
            std::io::stderr().flush().unwrap();
        }
        if role == "failure" {
            panic!("probe failure marker");
        }
        if role == "parent" {
            return;
        }
        std::fs::write(root.join("ready"), []).unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        while root.exists() && !root.join("release").is_file() {
            assert!(Instant::now() < deadline, "probe was not released");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_command_finishes_while_a_descendant_keeps_output_handles_open() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let output = spawn(root.path(), "parent")
            .wait(Duration::from_secs(30))
            .unwrap();
        assert!(String::from_utf8_lossy(&output).contains("probe stdout marker"));
        assert!(root.path().join("ready").is_file());
        assert!(!root.path().join("release").exists());
    }

    #[test]
    fn a_timeout_retains_partial_output_and_reaps_the_process() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let probe = spawn(root.path(), "waiting");
        ready(root.path());
        let error = format!("{:#}", probe.wait(Duration::ZERO).unwrap_err());
        assert!(error.contains("the probe timed out"), "{error}");
        assert!(error.contains("probe stdout marker"), "{error}");
        assert!(error.contains("probe stderr marker"), "{error}");
        assert!(error.contains("killed"), "{error}");
    }

    #[test]
    fn a_failure_retains_both_output_streams() {
        let root = tempfile::tempdir().unwrap();
        let error = spawn(root.path(), "failure")
            .wait(Duration::from_secs(30))
            .unwrap_err();
        let error = format!("{error:#}");
        assert!(error.contains("the probe exited with"), "{error}");
        assert!(error.contains("probe stdout marker"), "{error}");
        assert!(error.contains("probe stderr marker"), "{error}");
        assert!(error.contains("probe failure marker"), "{error}");
    }

    #[test]
    fn a_failing_exit_is_returned_with_its_output_when_asked_for() {
        let root = tempfile::tempdir().unwrap();
        let exited = spawn(root.path(), "failure")
            .exit(Duration::from_secs(30))
            .unwrap();
        assert!(!exited.status.success());
        assert!(String::from_utf8_lossy(&exited.stdout).contains("probe stdout marker"));
        assert!(String::from_utf8_lossy(&exited.stderr).contains("probe failure marker"));
    }

    #[test]
    fn a_timeout_names_the_output_and_the_exit_phase() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let probe = spawn(root.path(), "waiting");
        ready(root.path());
        let error = format!("{:#}", probe.wait(Duration::from_millis(250)).unwrap_err());
        assert!(error.contains("stdout complete at"), "{error}");
        assert!(error.contains("stdout is not JSON"), "{error}");
        assert!(error.contains("process still running at"), "{error}");
        assert!(error.contains("killed"), "{error}");
    }

    #[test]
    fn a_timeout_separates_absent_output_from_complete_output() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let probe = spawn(root.path(), "silent");
        ready(root.path());
        let error = format!("{:#}", probe.wait(Duration::from_millis(250)).unwrap_err());
        assert!(error.contains("no stdout at"), "{error}");
        assert!(!error.contains("stdout complete at"), "{error}");
        assert!(error.contains("process still running at"), "{error}");
    }

    #[test]
    fn a_timeout_announces_truncation_and_keeps_the_stderr_tail() {
        let root = tempfile::tempdir().unwrap();
        let _release = Release(root.path().to_owned());
        let probe = spawn(root.path(), "flooding");
        ready(root.path());
        let error = format!("{:#}", probe.wait(Duration::from_millis(250)).unwrap_err());
        assert!(error.contains("probe stderr tail marker"), "{error}");
        assert!(error.contains("bytes dropped"), "{error}");
    }
}
