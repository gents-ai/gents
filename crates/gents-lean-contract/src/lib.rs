use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN GENTS LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END GENTS LEAN CONTRACT JSON---";

const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";
const LAKE_BUILD_ATTEMPTS: usize = 3;

const LOCK_FILE_NAME: &str = "gents-lean-contract.lock";

static PROCESS_STDOUT_CACHE: OnceLock<String> = OnceLock::new();

static PROCESS_LOAD_MUTEX: Mutex<()> = Mutex::new(());

pub fn load_contract_snapshot<T>() -> Result<T>
where
    T: DeserializeOwned,
{
    let stdout = load_contract_stdout()?;
    let json = extract_contract_json(&stdout)?;
    serde_json::from_str(json).with_context(|| {
        format!("failed to parse Lean conformance contract JSON\nstdout:\n{stdout}")
    })
}

pub fn load_contract_json() -> Result<String> {
    let stdout = load_contract_stdout()?;
    extract_contract_json(&stdout).map(ToOwned::to_owned)
}

pub fn load_contract_stdout() -> Result<String> {
    load_contract_stdout_for(
        &proofs_dir()?,
        &PROCESS_STDOUT_CACHE,
        &PROCESS_LOAD_MUTEX,
        &run_lake_build_unlocked,
        &run_contract_generator_unlocked,
    )
}

fn load_contract_stdout_for(
    proofs_dir: &Path,
    cache: &OnceLock<String>,
    flight: &Mutex<()>,
    build: &dyn Fn(&Path) -> Result<()>,
    generate: &dyn Fn(&Path) -> Result<String>,
) -> Result<String> {
    if let Some(cached) = cache.get() {
        return Ok(cached.clone());
    }

    let _in_process = flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(cached) = cache.get() {
        return Ok(cached.clone());
    }

    let stdout = with_proofs_dir_lock(proofs_dir, || {
        build(proofs_dir)?;
        generate(proofs_dir)
    })?;
    let _ = cache.set(stdout.clone());
    Ok(stdout)
}

/// Run `lake build` for the contract target under the proofs-dir lock.
pub fn run_lake_build(proofs_dir: &Path) -> Result<()> {
    with_proofs_dir_lock(proofs_dir, || run_lake_build_unlocked(proofs_dir))
}

fn run_lake_build_unlocked(proofs_dir: &Path) -> Result<()> {
    let mut failures = Vec::new();

    for attempt in 1..=LAKE_BUILD_ATTEMPTS {
        let mut command = Command::new("lake");
        command
            .args(["build", CONTRACT_TARGET])
            .current_dir(proofs_dir)
            // Match `Command::output`: Lake must never inherit an interactive
            .stdin(Stdio::null());
        let output = run_with_visible_output(&mut command, io::stdout(), io::stderr())
            .with_context(|| {
                format!(
                    "failed to build Lean conformance contract target in {}",
                    proofs_dir.display()
                )
            })?;

        if output.status.success() {
            return Ok(());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let retryable = is_retryable_lake_build_failure(&stdout, &stderr);
        failures.push(format!(
            "attempt {attempt}/{LAKE_BUILD_ATTEMPTS}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            output.status, stdout, stderr
        ));

        if !retryable || attempt == LAKE_BUILD_ATTEMPTS {
            anyhow::bail!(
                "Lean conformance contract build failed\n  cwd: {}\n{}",
                proofs_dir.display(),
                failures.join("\n\n")
            );
        }

        std::thread::sleep(Duration::from_secs((attempt as u64) * 5));
    }

    unreachable!("lake build retry loop should return or bail")
}

type OutputTask = thread::JoinHandle<io::Result<Vec<u8>>>;

fn run_with_visible_output(
    command: &mut Command,
    stdout_destination: impl Write + Send + 'static,
    stderr_destination: impl Write + Send + 'static,
) -> Result<Output> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            terminate_and_reap_child(&mut child, Vec::new());
            return Err(anyhow!("visible command stdout pipe was unavailable"));
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdout);
            terminate_and_reap_child(&mut child, Vec::new());
            return Err(anyhow!("visible command stderr pipe was unavailable"));
        }
    };

    let stdout_task = match spawn_output_task("stdout", stdout, stdout_destination) {
        Ok(task) => task,
        Err(error) => {
            drop(stderr);
            terminate_and_reap_child(&mut child, Vec::new());
            return Err(error).context("failed to spawn command stdout forwarding thread");
        }
    };
    let stderr_task = match spawn_output_task("stderr", stderr, stderr_destination) {
        Ok(task) => task,
        Err(error) => {
            terminate_and_reap_child(&mut child, vec![stdout_task]);
            return Err(error).context("failed to spawn command stderr forwarding thread");
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            terminate_and_reap_child(&mut child, vec![stdout_task, stderr_task]);
            return Err(error.into());
        }
    };
    let stdout = join_output_task(stdout_task, "stdout");
    let stderr = join_output_task(stderr_task, "stderr");
    let stdout = stdout?;
    let stderr = stderr?;

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_output_task(
    stream: &'static str,
    source: impl Read + Send + 'static,
    destination: impl Write + Send + 'static,
) -> io::Result<OutputTask> {
    thread::Builder::new()
        .name(format!("lean-contract-{stream}"))
        .spawn(move || forward_and_capture(source, destination))
}

/// must not replace it or make a Lake invocation eligible for different retry
fn terminate_and_reap_child(child: &mut Child, tasks: Vec<OutputTask>) {
    let _ = child.kill();
    let _ = child.wait();
    for task in tasks {
        let _ = task.join();
    }
}

fn forward_and_capture(mut source: impl Read, mut destination: impl Write) -> io::Result<Vec<u8>> {
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8 * 1024];
    loop {
        let read = match source.read(&mut buffer) {
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        captured.extend_from_slice(chunk);
        // Observability must not change command success semantics if a caller
        let _ = destination.write_all(chunk);
        let _ = destination.flush();
    }
    Ok(captured)
}

fn join_output_task(
    task: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: &str,
) -> Result<Vec<u8>> {
    match task.join() {
        Ok(result) => result.map_err(Into::into),
        Err(_) => Err(anyhow!("{stream} forwarding thread panicked")),
    }
}

fn is_retryable_lake_build_failure(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    combined.contains("external command 'git' exited with code 128")
        || combined.contains("failed to fetch GitHub release")
        || combined.contains("ProofWidgets not up-to-date")
}

/// Run the contract generator under the proofs-dir lock.
pub fn run_contract_generator(proofs_dir: &Path) -> Result<String> {
    with_proofs_dir_lock(proofs_dir, || run_contract_generator_unlocked(proofs_dir))
}

fn run_contract_generator_unlocked(proofs_dir: &Path) -> Result<String> {
    let output = Command::new("lake")
        .args(["env", "lean", "--run", CONTRACT_RUN_FILE])
        .current_dir(proofs_dir)
        .output()
        .with_context(|| {
            format!(
                "failed to run Lean conformance contract generator in {}",
                proofs_dir.display()
            )
        })?;

    if !output.status.success() {
        anyhow::bail!(
            "Lean conformance contract generator failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    String::from_utf8(output.stdout).context("Lean conformance contract stdout was not UTF-8")
}

fn with_proofs_dir_lock<T>(proofs_dir: &Path, op: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = lock_path_for(proofs_dir)?;
    let _guard = acquire_exclusive_lock(&lock_path)?;
    op().with_context(|| {
        format!(
            "while holding exclusive Lean contract load lock\n  lock: {}\n  proofs: {}",
            lock_path.display(),
            proofs_dir.display()
        )
    })
}

fn lock_path_for(proofs_dir: &Path) -> Result<PathBuf> {
    let canonical = canonicalize_proofs_dir(proofs_dir)?;
    Ok(canonical.join(".lake").join(LOCK_FILE_NAME))
}

fn canonicalize_proofs_dir(proofs_dir: &Path) -> Result<PathBuf> {
    match std::fs::canonicalize(proofs_dir) {
        Ok(path) => Ok(path),
        Err(err) => {
            let absolute = if proofs_dir.is_absolute() {
                proofs_dir.to_path_buf()
            } else {
                std::env::current_dir()
                    .with_context(|| {
                        format!(
                            "failed to resolve proofs dir {} (canonicalize: {err})",
                            proofs_dir.display()
                        )
                    })?
                    .join(proofs_dir)
            };
            Ok(absolute)
        }
    }
}

fn acquire_exclusive_lock(lock_path: &Path) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Lean contract load lock directory {}",
                parent.display()
            )
        })?;
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .with_context(|| {
            format!(
                "failed to open Lean contract load lock at {}",
                lock_path.display()
            )
        })?;

    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            file.lock().with_context(|| {
                format!(
                    "failed waiting for exclusive Lean contract load lock\n  lock: {}\n  hint: another process is running lake build / contract generation for this proofs directory",
                    lock_path.display()
                )
            })?;
            Ok(file)
        }
        Err(TryLockError::Error(err)) => Err(err).with_context(|| {
            format!(
                "failed to acquire exclusive Lean contract load lock at {}",
                lock_path.display()
            )
        }),
    }
}

pub fn proofs_dir() -> Result<PathBuf> {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));

    let direct = manifest_dir.join("proofs");
    if direct.join("lakefile.lean").exists() {
        return Ok(direct);
    }

    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("crates/gents/proofs");
        if candidate.join("lakefile.lean").exists() {
            return Ok(candidate);
        }
    }

    let sibling = manifest_dir
        .parent()
        .map(|parent| parent.join("gents/proofs"));
    if let Some(candidate) = sibling {
        if candidate.join("lakefile.lean").exists() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "could not locate crates/gents/proofs from {}",
        manifest_dir.display()
    ))
}

pub fn extract_contract_json(stdout: &str) -> Result<&str> {
    let begin = unique_marker_position(stdout, CONTRACT_JSON_BEGIN)?;
    let end = unique_marker_position(stdout, CONTRACT_JSON_END)?;
    anyhow::ensure!(
        begin < end,
        "Lean contract JSON sentinel order is invalid\n  stdout:\n{}",
        stdout
    );

    let json = stdout[begin + CONTRACT_JSON_BEGIN.len()..end].trim();
    anyhow::ensure!(
        !json.is_empty(),
        "Lean contract JSON sentinel block was empty\n  stdout:\n{}",
        stdout
    );
    Ok(json)
}

fn unique_marker_position(stdout: &str, marker: &str) -> Result<usize> {
    let positions = stdout
        .match_indices(marker)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => Ok(*position),
        [] => Err(anyhow!(
            "Lean contract generator stdout did not contain {marker:?}: {stdout}"
        )),
        _ => Err(anyhow!(
            "Lean contract generator stdout contained duplicate {marker:?} sentinels: {stdout}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn extracts_contract_json_between_unique_sentinels() {
        let stdout = format!(
            "debug {{noise}}\n{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_END}\nmore {{noise}}\n"
        );

        assert_eq!(extract_contract_json(&stdout).unwrap(), "{\"ok\":true}");
    }

    #[test]
    fn rejects_duplicate_sentinels() {
        let stdout = format!(
            "{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_BEGIN}\n{CONTRACT_JSON_END}\n"
        );

        let err = extract_contract_json(&stdout).unwrap_err().to_string();
        assert!(
            err.contains("duplicate"),
            "expected duplicate sentinel error, got: {err}"
        );
    }

    #[test]
    fn retries_transient_lake_dependency_fetch_failures() {
        assert!(is_retryable_lake_build_failure(
            "",
            "info: mathlib: cloning https://github.com/leanprover-community/mathlib4\n\
             error: external command 'git' exited with code 128"
        ));
    }

    #[test]
    fn does_not_retry_ordinary_lean_build_errors() {
        assert!(!is_retryable_lake_build_failure(
            "",
            "error: Proofs/Foo.lean:12:4: unknown identifier 'bar'"
        ));
    }

    #[test]
    fn forwarding_command_output_retains_exact_diagnostics() {
        let input = b"cold Lean build progress\n";
        let mut forwarded = Vec::new();

        let captured = forward_and_capture(input.as_slice(), &mut forwarded).unwrap();

        assert_eq!(captured, input);
        assert_eq!(forwarded, input);
    }

    #[test]
    fn forwarding_command_output_retries_interrupted_reads() {
        struct InterruptedOnce<'a> {
            input: &'a [u8],
            interrupted: bool,
        }

        impl Read for InterruptedOnce<'_> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::Error::from(io::ErrorKind::Interrupted));
                }
                self.input.read(buffer)
            }
        }

        let input = b"resumed output\n";
        let mut forwarded = Vec::new();
        let captured = forward_and_capture(
            InterruptedOnce {
                input,
                interrupted: false,
            },
            &mut forwarded,
        )
        .unwrap();

        assert_eq!(captured, input);
        assert_eq!(forwarded, input);
    }

    #[test]
    fn forwarding_command_output_preserves_raw_read_error() {
        struct FailingReader;

        impl Read for FailingReader {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::other("sentinel pipe read failure"))
            }
        }

        let task = thread::spawn(|| forward_and_capture(FailingReader, io::sink()));
        let error = join_output_task(task, "stdout").unwrap_err();
        let chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

        assert_eq!(error.to_string(), "sentinel pipe read failure");
        assert_eq!(format!("{error:#}"), "sentinel pipe read failure");
        assert_eq!(chain, vec!["sentinel pipe read failure"]);
        assert_eq!(
            error.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::Other)
        );
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct CapturingWriter(Arc<Mutex<Vec<u8>>>);

    #[cfg(unix)]
    impl CapturingWriter {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }
    }

    #[cfg(unix)]
    impl Write for CapturingWriter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    #[test]
    fn visible_command_honors_configured_stdin_and_preserves_separate_nonzero_output() {
        let fixture = unique_temp_dir("visible-command-stdin");
        let stdin_path = fixture.join("stdin");
        std::fs::write(&stdin_path, b"configured stdin\n").unwrap();

        let forwarded_stdout = CapturingWriter::default();
        let forwarded_stderr = CapturingWriter::default();
        let mut command = Command::new("sh");
        command
            .args([
                "-c",
                "IFS= read -r line || exit 91\n\
                 printf 'stdin=%s\\n' \"$line\"\n\
                 printf 'stdout-only\\n'\n\
                 printf 'stderr-only\\n' >&2\n\
                 exit 23",
            ])
            .stdin(File::open(&stdin_path).unwrap());

        let output = run_with_visible_output(
            &mut command,
            forwarded_stdout.clone(),
            forwarded_stderr.clone(),
        )
        .unwrap();

        assert_eq!(output.status.code(), Some(23));
        assert_eq!(output.stdout, b"stdin=configured stdin\nstdout-only\n");
        assert_eq!(output.stderr, b"stderr-only\n");
        assert_eq!(forwarded_stdout.bytes(), output.stdout);
        assert_eq!(forwarded_stderr.bytes(), output.stderr);

        let _ = std::fs::remove_dir_all(fixture);
    }

    #[cfg(unix)]
    #[test]
    fn visible_command_drains_both_streams_beyond_pipe_capacity() {
        const REPETITIONS: usize = 4_096;
        const STDOUT_CHUNK: &str = "stdout-0123456789abcdef-0123456789abcdef-0123456789abcdef\n";
        const STDERR_CHUNK: &str = "stderr-fedcba9876543210-fedcba9876543210-fedcba9876543210\n";

        let script = format!(
            "i=0\n\
             while [ \"$i\" -lt {REPETITIONS} ]; do\n\
               printf '%s' '{STDOUT_CHUNK}'\n\
               i=$((i + 1))\n\
             done &\n\
             stdout_pid=$!\n\
             i=0\n\
             while [ \"$i\" -lt {REPETITIONS} ]; do\n\
               printf '%s' '{STDERR_CHUNK}' >&2\n\
               i=$((i + 1))\n\
             done &\n\
             stderr_pid=$!\n\
             wait \"$stdout_pid\"\n\
             wait \"$stderr_pid\"\n"
        );
        let forwarded_stdout = CapturingWriter::default();
        let forwarded_stderr = CapturingWriter::default();
        let mut command = Command::new("sh");
        command.args(["-c", &script]);

        let output = run_with_visible_output(
            &mut command,
            forwarded_stdout.clone(),
            forwarded_stderr.clone(),
        )
        .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, STDOUT_CHUNK.as_bytes().repeat(REPETITIONS));
        assert_eq!(output.stderr, STDERR_CHUNK.as_bytes().repeat(REPETITIONS));
        assert_eq!(forwarded_stdout.bytes(), output.stdout);
        assert_eq!(forwarded_stderr.bytes(), output.stderr);
    }

    #[cfg(unix)]
    struct FailingWriter;

    #[cfg(unix)]
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }

    #[cfg(unix)]
    #[test]
    fn visible_command_ignores_forwarding_destination_failures() {
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf 'captured stdout\\n'; printf 'captured stderr\\n' >&2",
        ]);

        let output = run_with_visible_output(&mut command, FailingWriter, FailingWriter).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"captured stdout\n");
        assert_eq!(output.stderr, b"captured stderr\n");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "gents-lean-contract-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn concurrent_threads_single_flight_same_payload() {
        let proofs_dir = unique_temp_dir("single-flight");
        // Shared cache/flight: same production wiring as load_contract_stdout.
        let cache = OnceLock::new();
        let flight = Mutex::new(());
        let build_count = AtomicUsize::new(0);
        let generate_count = AtomicUsize::new(0);
        let payload =
            format!("{CONTRACT_JSON_BEGIN}\n{{\"thread_test\":true}}\n{CONTRACT_JSON_END}\n");

        let build = |_: &Path| -> Result<()> {
            build_count.fetch_add(1, Ordering::SeqCst);
            // Hold the critical section long enough for other threads to pile up.
            thread::sleep(Duration::from_millis(100));
            Ok(())
        };
        let generate = |_: &Path| -> Result<String> {
            generate_count.fetch_add(1, Ordering::SeqCst);
            Ok(payload.clone())
        };

        thread::scope(|scope| {
            let mut handles = Vec::new();
            for _ in 0..8 {
                handles.push(scope.spawn(|| {
                    load_contract_stdout_for(&proofs_dir, &cache, &flight, &build, &generate)
                        .unwrap()
                }));
            }
            let results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
            for result in &results {
                assert_eq!(result, &payload);
            }
        });

        assert_eq!(
            build_count.load(Ordering::SeqCst),
            1,
            "build should run once under single-flight"
        );
        assert_eq!(
            generate_count.load(Ordering::SeqCst),
            1,
            "generator should run once under single-flight"
        );

        let _ = std::fs::remove_dir_all(&proofs_dir);
    }

    #[test]
    fn failed_protected_operation_releases_lock_for_next_caller() {
        let proofs_dir = unique_temp_dir("fail-release");
        let lock_path = lock_path_for(&proofs_dir).unwrap();
        let flight = Mutex::new(());

        // Failures must not be cached, so each attempt uses a fresh OnceLock.
        let err = load_contract_stdout_for(
            &proofs_dir,
            &OnceLock::new(),
            &flight,
            &|_| anyhow::bail!("intentional protected failure"),
            &|_| unreachable!("generate must not run after build fails"),
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(
            err.contains("intentional protected failure"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains(lock_path.to_string_lossy().as_ref())
                || err.contains("while holding exclusive Lean contract load lock"),
            "failure should retain lock diagnostics, got: {err}"
        );

        // Lock must be free immediately for the next production-core caller.
        load_contract_stdout_for(&proofs_dir, &OnceLock::new(), &flight, &|_| Ok(()), &|_| {
            Ok(String::from("ok"))
        })
        .unwrap();

        let _ = std::fs::remove_dir_all(&proofs_dir);
    }

    #[test]
    fn different_proof_directories_do_not_share_a_lock() {
        let dir_a = unique_temp_dir("lock-a");
        let dir_b = unique_temp_dir("lock-b");
        let lock_a = lock_path_for(&dir_a).unwrap();
        let lock_b = lock_path_for(&dir_b).unwrap();
        assert_ne!(
            lock_a, lock_b,
            "distinct proof checkouts need distinct locks"
        );

        // Hold A's lock via the production load path (build blocks until released).
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        let dir_a_thread = dir_a.clone();
        let holder = thread::spawn(move || {
            load_contract_stdout_for(
                &dir_a_thread,
                &OnceLock::new(),
                &Mutex::new(()),
                &|_| {
                    held_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                &|_| Ok(String::from("a")),
            )
            .unwrap()
        });
        held_rx.recv().unwrap();

        // B must still load while A is held.
        load_contract_stdout_for(
            &dir_b,
            &OnceLock::new(),
            &Mutex::new(()),
            &|_| Ok(()),
            &|_| Ok(String::from("b")),
        )
        .unwrap();

        // Same directory must contend while A is held.
        let contended = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_a)
            .unwrap();
        match contended.try_lock() {
            Err(TryLockError::WouldBlock) => {}
            other => panic!("expected WouldBlock on same proofs lock, got {other:?}"),
        }

        release_tx.send(()).unwrap();
        holder.join().unwrap();

        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn same_canonical_proofs_dir_shares_lock_path() {
        let dir = unique_temp_dir("canonical");
        // Create a real directory so canonicalize succeeds.
        let via_dot = dir.join(".");
        let path_a = lock_path_for(&dir).unwrap();
        let path_b = lock_path_for(&via_dot).unwrap();
        assert_eq!(
            path_a, path_b,
            "paths that resolve to the same checkout must share a lock"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lock_open_failure_includes_lock_path() {
        let base = unique_temp_dir("lock-open-fail");
        // Make `.lake` a file so create_dir_all / open of the lock path fails.
        let lake_as_file = base.join(".lake");
        std::fs::write(&lake_as_file, b"not a directory").unwrap();

        let err = load_contract_stdout_for(
            &base,
            &OnceLock::new(),
            &Mutex::new(()),
            &|_| Ok(()),
            &|_| Ok(String::from("unused")),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("lock") || err.contains(".lake"),
            "expected lock-path diagnostics, got: {err}"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn protected_op_failure_includes_lock_and_proofs_context() {
        let proofs_dir = unique_temp_dir("diag");
        let lock_path = lock_path_for(&proofs_dir).unwrap();
        let err = load_contract_stdout_for(
            &proofs_dir,
            &OnceLock::new(),
            &Mutex::new(()),
            &|dir| {
                anyhow::bail!(
                    "Lean conformance contract build failed\n  cwd: {}\n  status: exit 1\n  stderr:\nbogus",
                    dir.display()
                )
            },
            &|_| unreachable!("generate must not run after build fails"),
        )
        .unwrap_err();
        let err = format!("{err:#}");

        assert!(
            err.contains("Lean conformance contract build failed"),
            "original lake diagnostics must be preserved: {err}"
        );
        assert!(
            err.contains("while holding exclusive Lean contract load lock"),
            "lock context missing: {err}"
        );
        assert!(
            err.contains(&lock_path.display().to_string())
                || err.contains(&proofs_dir.display().to_string()),
            "lock/proofs paths missing: {err}"
        );

        let _ = std::fs::remove_dir_all(&proofs_dir);
    }

    #[test]
    fn cross_process_mutations_do_not_overlap() {
        const ENV_FIXTURE: &str = "GENTS_LEAN_CONTRACT_CROSS_PROCESS_FIXTURE";
        const CHILDREN: usize = 4;
        const HOLD_MS: u64 = 150;

        if let Ok(fixture) = std::env::var(ENV_FIXTURE) {
            let fixture = PathBuf::from(fixture);
            let proofs_dir = fixture.join("proofs");
            let holder_path = fixture.join("holder");
            let overlap_path = fixture.join("overlap");
            let done_dir = fixture.join("done");

            // Drive the production load core so a disconnect from the lock fails.
            let build = |_: &Path| -> Result<()> {
                if holder_path.exists() {
                    std::fs::write(&overlap_path, b"overlap detected").ok();
                    anyhow::bail!("overlapping critical section: holder already present");
                }
                std::fs::write(&holder_path, std::process::id().to_string())?;
                thread::sleep(Duration::from_millis(HOLD_MS));
                let held_by = std::fs::read_to_string(&holder_path)?;
                if held_by.trim() != std::process::id().to_string() {
                    std::fs::write(&overlap_path, b"holder stolen").ok();
                    anyhow::bail!("holder rewritten by another process: {held_by}");
                }
                std::fs::remove_file(&holder_path)?;
                std::fs::write(done_dir.join(std::process::id().to_string()), b"ok")?;
                Ok(())
            };
            load_contract_stdout_for(
                &proofs_dir,
                &OnceLock::new(),
                &Mutex::new(()),
                &build,
                &|_| Ok(String::from("child-payload")),
            )
            .expect("child critical section");
            return;
        }

        let fixture = unique_temp_dir("cross-process");
        let proofs_dir = fixture.join("proofs");
        let done_dir = fixture.join("done");
        std::fs::create_dir_all(&proofs_dir).unwrap();
        std::fs::create_dir_all(&done_dir).unwrap();

        let exe = std::env::current_exe().expect("current test binary");
        let mut children = Vec::new();
        for _ in 0..CHILDREN {
            let child = std::process::Command::new(&exe)
                // Full libtest path so the child actually executes this test body.
                .arg("tests::cross_process_mutations_do_not_overlap")
                .arg("--exact")
                .arg("--nocapture")
                .env(ENV_FIXTURE, &fixture)
                .env("RUST_BACKTRACE", "0")
                .spawn()
                .expect("spawn lock child");
            children.push(child);
        }

        let mut failures = Vec::new();
        for (idx, child) in children.into_iter().enumerate() {
            let output = child.wait_with_output().expect("wait child");
            if !output.status.success() {
                failures.push(format!(
                    "child {idx} failed: {}\nstdout:\n{}\nstderr:\n{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }

        let completed = std::fs::read_dir(&done_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .count();
        assert_eq!(
            completed, CHILDREN,
            "expected {CHILDREN} child critical-section completions, got {completed}"
        );

        let overlap = fixture.join("overlap");
        assert!(
            !overlap.exists(),
            "processes overlapped inside the protected mutation"
        );
        assert!(
            failures.is_empty(),
            "cross-process lock children failed:\n{}",
            failures.join("\n\n")
        );

        let _ = std::fs::remove_dir_all(&fixture);
    }

    #[test]
    fn concurrent_threads_under_lock_never_overlap_mutation() {
        let proofs_dir = unique_temp_dir("thread-overlap");
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

        // Independent caches so each thread enters the production locked path
        // rather than collapsing into single-flight.
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in 0..6 {
                let proofs_dir = &proofs_dir;
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                handles.push(scope.spawn(move || {
                    let build = |_: &Path| -> Result<()> {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    };
                    load_contract_stdout_for(
                        proofs_dir,
                        &OnceLock::new(),
                        &Mutex::new(()),
                        &build,
                        &|_| Ok(i.to_string()),
                    )
                    .unwrap()
                }));
            }
            for handle in handles {
                handle.join().unwrap();
            }
        });

        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "exclusive proofs lock must serialize the protected mutation"
        );
        let _ = std::fs::remove_dir_all(&proofs_dir);
    }
}
