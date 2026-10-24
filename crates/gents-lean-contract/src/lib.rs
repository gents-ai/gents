use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use serde::de::DeserializeOwned;

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN GENTS LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END GENTS LEAN CONTRACT JSON---";

const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";
const LAKE_BUILD_ATTEMPTS: usize = 3;

const LOCK_FILE_NAME: &str = "gents-lean-contract.lock";

static PROCESS_STDOUT_CACHE: OnceLock<(PathBuf, String)> = OnceLock::new();

static PROCESS_LOAD_MUTEX: Mutex<()> = Mutex::new(());

#[derive(Debug, thiserror::Error)]
pub enum LeanContractError {
    #[error("Lean contract cache belongs to another proofs directory")]
    CacheRootMismatch,
    #[error("failed to parse Lean conformance contract JSON\nstdout:\n{stdout}")]
    ParseJson {
        stdout: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to build Lean conformance contract target in {}", proofs_dir.display())]
    RunBuild {
        proofs_dir: PathBuf,
        #[source]
        source: Box<LeanContractError>,
    },
    #[error("Lean conformance contract build failed\n  cwd: {}\n{failures}", cwd.display())]
    BuildFailed { cwd: PathBuf, failures: String },
    #[error("visible command stdout pipe was unavailable")]
    StdoutPipeUnavailable,
    #[error("visible command stderr pipe was unavailable")]
    StderrPipeUnavailable,
    #[error("failed to spawn command {stream} forwarding thread")]
    SpawnForwarding {
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("{stream} forwarding thread panicked")]
    ForwardingPanicked { stream: &'static str },
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("failed to run Lean conformance contract generator in {}", proofs_dir.display())]
    RunGenerator {
        proofs_dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "Lean conformance contract generator failed\n  cwd: {}\n  status: {status}\n  stdout:\n{stdout}\n  stderr:\n{stderr}",
        cwd.display()
    )]
    GeneratorFailed {
        cwd: PathBuf,
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },
    #[error("Lean conformance contract stdout was not UTF-8")]
    NonUtf8Stdout(#[source] std::string::FromUtf8Error),
    #[error(
        "while holding exclusive Lean contract load lock\n  lock: {}\n  proofs: {}",
        lock_path.display(),
        proofs_dir.display()
    )]
    LockedOperation {
        lock_path: PathBuf,
        proofs_dir: PathBuf,
        #[source]
        source: Box<LeanContractError>,
    },
    #[error("failed to resolve proofs dir {} (canonicalize: {canonicalize})", proofs_dir.display())]
    ResolveProofsDir {
        proofs_dir: PathBuf,
        canonicalize: io::Error,
        #[source]
        source: io::Error,
    },
    #[error("failed to create Lean contract load lock directory {}", dir.display())]
    CreateLockDir {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to open Lean contract load lock at {}", lock_path.display())]
    OpenLock {
        lock_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "failed waiting for exclusive Lean contract load lock\n  lock: {}\n  hint: another process is running lake build / contract generation for this proofs directory",
        lock_path.display()
    )]
    WaitLock {
        lock_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to acquire exclusive Lean contract load lock at {}", lock_path.display())]
    AcquireLock {
        lock_path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("missing runtime {var} for Lean contract source")]
    MissingManifestEnv { var: &'static str },
    #[error("runtime Cargo manifest paths must be absolute")]
    RelativeManifestPaths,
    #[error("invalid runtime {var}")]
    InvalidManifestPath {
        var: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("runtime Cargo manifest paths disagree")]
    ManifestPathsDisagree,
    #[error("could not locate crates/gents/proofs from {}", manifest_dir.display())]
    ProofsDirNotFound { manifest_dir: PathBuf },
    #[error("Lean contract JSON sentinel order is invalid\n  stdout:\n{stdout}")]
    SentinelOrder { stdout: String },
    #[error("Lean contract JSON sentinel block was empty\n  stdout:\n{stdout}")]
    EmptySentinelBlock { stdout: String },
    #[error("Lean contract generator stdout did not contain {marker:?}: {stdout}")]
    MissingSentinel {
        marker: &'static str,
        stdout: String,
    },
    #[error("Lean contract generator stdout contained duplicate {marker:?} sentinels: {stdout}")]
    DuplicateSentinel {
        marker: &'static str,
        stdout: String,
    },
}

pub type Result<T, E = LeanContractError> = std::result::Result<T, E>;

pub fn load_contract_snapshot<T>() -> Result<T>
where
    T: DeserializeOwned,
{
    let stdout = load_contract_stdout()?;
    let json = extract_contract_json(&stdout)?;
    serde_json::from_str(json).map_err(|source| LeanContractError::ParseJson {
        stdout: stdout.clone(),
        source,
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
    cache: &OnceLock<(PathBuf, String)>,
    flight: &Mutex<()>,
    build: &dyn Fn(&Path) -> Result<()>,
    generate: &dyn Fn(&Path) -> Result<String>,
) -> Result<String> {
    let proofs_dir = canonicalize_proofs_dir(proofs_dir)?;
    if let Some((root, cached)) = cache.get() {
        if root != &proofs_dir {
            return Err(LeanContractError::CacheRootMismatch);
        }
        return Ok(cached.clone());
    }

    let _in_process = flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some((root, cached)) = cache.get() {
        if root != &proofs_dir {
            return Err(LeanContractError::CacheRootMismatch);
        }
        return Ok(cached.clone());
    }

    let stdout = with_proofs_dir_lock(&proofs_dir, || {
        build(&proofs_dir)?;
        generate(&proofs_dir)
    })?;
    let _ = cache.set((proofs_dir, stdout.clone()));
    Ok(stdout)
}

pub fn run_lake_build(proofs_dir: &Path) -> Result<()> {
    with_proofs_dir_lock(proofs_dir, || run_lake_build_unlocked(proofs_dir))
}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous build driver outside any runtime; backs off between blocking lake runs"
)]
fn run_lake_build_unlocked(proofs_dir: &Path) -> Result<()> {
    let mut failures = Vec::new();

    for attempt in 1..=LAKE_BUILD_ATTEMPTS {
        let mut command = Command::new("lake");
        command
            .args(["build", CONTRACT_TARGET])
            .current_dir(proofs_dir)
            .stdin(Stdio::null());
        let output = run_with_visible_output(&mut command, io::stdout(), io::stderr()).map_err(
            |source| LeanContractError::RunBuild {
                proofs_dir: proofs_dir.to_path_buf(),
                source: Box::new(source),
            },
        )?;

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
            return Err(LeanContractError::BuildFailed {
                cwd: proofs_dir.to_path_buf(),
                failures: failures.join("\n\n"),
            });
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
            return Err(LeanContractError::StdoutPipeUnavailable);
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            drop(stdout);
            terminate_and_reap_child(&mut child, Vec::new());
            return Err(LeanContractError::StderrPipeUnavailable);
        }
    };

    let stdout_task = match spawn_output_task("stdout", stdout, stdout_destination) {
        Ok(task) => task,
        Err(error) => {
            drop(stderr);
            terminate_and_reap_child(&mut child, Vec::new());
            return Err(LeanContractError::SpawnForwarding {
                stream: "stdout",
                source: error,
            });
        }
    };
    let stderr_task = match spawn_output_task("stderr", stderr, stderr_destination) {
        Ok(task) => task,
        Err(error) => {
            terminate_and_reap_child(&mut child, vec![stdout_task]);
            return Err(LeanContractError::SpawnForwarding {
                stream: "stderr",
                source: error,
            });
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

/// Termination must not replace the original setup/wait error or change
/// eligibility for lake-build retry behavior.
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
        // A caller may close its output stream early; observability must not
        // change command success semantics.
        let _ = destination.write_all(chunk);
        let _ = destination.flush();
    }
    Ok(captured)
}

fn join_output_task(
    task: thread::JoinHandle<io::Result<Vec<u8>>>,
    stream: &'static str,
) -> Result<Vec<u8>> {
    match task.join() {
        Ok(result) => result.map_err(Into::into),
        Err(_) => Err(LeanContractError::ForwardingPanicked { stream }),
    }
}

fn is_retryable_lake_build_failure(stdout: &str, stderr: &str) -> bool {
    let combined = format!("{stdout}\n{stderr}");
    combined.contains("external command 'git' exited with code 128")
        || combined.contains("failed to fetch GitHub release")
        || combined.contains("ProofWidgets not up-to-date")
}

pub fn run_contract_generator(proofs_dir: &Path) -> Result<String> {
    with_proofs_dir_lock(proofs_dir, || run_contract_generator_unlocked(proofs_dir))
}

fn run_contract_generator_unlocked(proofs_dir: &Path) -> Result<String> {
    let output = Command::new("lake")
        .args(["env", "lean", "--run", CONTRACT_RUN_FILE])
        .current_dir(proofs_dir)
        .output()
        .map_err(|source| LeanContractError::RunGenerator {
            proofs_dir: proofs_dir.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        return Err(LeanContractError::GeneratorFailed {
            cwd: proofs_dir.to_path_buf(),
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    String::from_utf8(output.stdout).map_err(LeanContractError::NonUtf8Stdout)
}

fn with_proofs_dir_lock<T>(proofs_dir: &Path, op: impl FnOnce() -> Result<T>) -> Result<T> {
    let lock_path = lock_path_for(proofs_dir)?;
    let _guard = acquire_exclusive_lock(&lock_path)?;
    op().map_err(|source| LeanContractError::LockedOperation {
        lock_path,
        proofs_dir: proofs_dir.to_path_buf(),
        source: Box::new(source),
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
                    .map_err(|source| LeanContractError::ResolveProofsDir {
                        proofs_dir: proofs_dir.to_path_buf(),
                        canonicalize: err,
                        source,
                    })?
                    .join(proofs_dir)
            };
            Ok(absolute)
        }
    }
}

fn acquire_exclusive_lock(lock_path: &Path) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LeanContractError::CreateLockDir {
            dir: parent.to_path_buf(),
            source,
        })?;
    }

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|source| LeanContractError::OpenLock {
            lock_path: lock_path.to_path_buf(),
            source,
        })?;

    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            file.lock().map_err(|source| LeanContractError::WaitLock {
                lock_path: lock_path.to_path_buf(),
                source,
            })?;
            Ok(file)
        }
        Err(TryLockError::Error(source)) => Err(LeanContractError::AcquireLock {
            lock_path: lock_path.to_path_buf(),
            source,
        }),
    }
}

/// Cargo supplies the consuming package's manifest paths at runtime; direct
/// execution must provide the same explicit anchor, and repository metadata
/// bounds discovery to the selected checkout.
pub fn proofs_dir() -> Result<PathBuf> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or(LeanContractError::MissingManifestEnv {
            var: "CARGO_MANIFEST_DIR",
        })?;
    let manifest_path = std::env::var_os("CARGO_MANIFEST_PATH")
        .map(PathBuf::from)
        .ok_or(LeanContractError::MissingManifestEnv {
            var: "CARGO_MANIFEST_PATH",
        })?;
    if !(manifest_dir.is_absolute() && manifest_path.is_absolute()) {
        return Err(LeanContractError::RelativeManifestPaths);
    }
    let manifest_dir = std::fs::canonicalize(&manifest_dir).map_err(|source| {
        LeanContractError::InvalidManifestPath {
            var: "CARGO_MANIFEST_DIR",
            source,
        }
    })?;
    let manifest_path = std::fs::canonicalize(&manifest_path).map_err(|source| {
        LeanContractError::InvalidManifestPath {
            var: "CARGO_MANIFEST_PATH",
            source,
        }
    })?;
    if !(manifest_path.is_file()
        && manifest_path
            .file_name()
            .is_some_and(|name| name == "Cargo.toml")
        && manifest_path.parent() == Some(manifest_dir.as_path()))
    {
        return Err(LeanContractError::ManifestPathsDisagree);
    }

    let repository_root = manifest_dir.ancestors().find(|ancestor| {
        let metadata = ancestor.join(".git");
        metadata.is_file() || metadata.is_dir()
    });
    let direct = manifest_dir.join("proofs");
    if direct.join("lakefile.lean").exists() {
        return Ok(direct);
    }

    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("crates/gents/proofs");
        if candidate.join("lakefile.lean").exists() {
            return Ok(candidate);
        }
        if repository_root == Some(ancestor) {
            break;
        }
    }

    let sibling = manifest_dir
        .parent()
        .map(|parent| parent.join("gents/proofs"));
    if let Some(candidate) = sibling {
        if repository_root.is_none_or(|root| candidate.starts_with(root))
            && candidate.join("lakefile.lean").exists()
        {
            return Ok(candidate);
        }
    }

    Err(LeanContractError::ProofsDirNotFound { manifest_dir })
}

pub fn extract_contract_json(stdout: &str) -> Result<&str> {
    let begin = unique_marker_position(stdout, CONTRACT_JSON_BEGIN)?;
    let end = unique_marker_position(stdout, CONTRACT_JSON_END)?;
    if begin >= end {
        return Err(LeanContractError::SentinelOrder {
            stdout: stdout.to_owned(),
        });
    }

    let json = stdout[begin + CONTRACT_JSON_BEGIN.len()..end].trim();
    if json.is_empty() {
        return Err(LeanContractError::EmptySentinelBlock {
            stdout: stdout.to_owned(),
        });
    }
    Ok(json)
}

fn unique_marker_position(stdout: &str, marker: &'static str) -> Result<usize> {
    let positions = stdout
        .match_indices(marker)
        .map(|(position, _)| position)
        .collect::<Vec<_>>();
    match positions.as_slice() {
        [position] => Ok(*position),
        [] => Err(LeanContractError::MissingSentinel {
            marker,
            stdout: stdout.to_owned(),
        }),
        _ => Err(LeanContractError::DuplicateSentinel {
            marker,
            stdout: stdout.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn chain(error: &dyn std::error::Error) -> String {
        std::iter::successors(Some(error), |error| error.source())
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(": ")
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
    fn cached_contract_rejects_another_proofs_root() {
        let first = unique_temp_dir("cache-source-a");
        let second = unique_temp_dir("cache-source-b");
        let cache = OnceLock::new();
        let flight = Mutex::new(());
        let payload = load_contract_stdout_for(&first, &cache, &flight, &|_| Ok(()), &|_| {
            Ok("first source".into())
        })
        .unwrap();
        assert_eq!(payload, "first source");
        let error = load_contract_stdout_for(
            &second,
            &cache,
            &flight,
            &|_| panic!("another root must not build"),
            &|_| panic!("another root must not generate"),
        )
        .unwrap_err();
        assert!(error.to_string().contains("another proofs directory"));
        std::fs::remove_dir_all(first).unwrap();
        std::fs::remove_dir_all(second).unwrap();
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "OS threads hold the in-process flight for real time"
    )]
    fn concurrent_threads_single_flight_same_payload() {
        let proofs_dir = unique_temp_dir("single-flight");
        let cache = OnceLock::new();
        let flight = Mutex::new(());
        let build_count = AtomicUsize::new(0);
        let generate_count = AtomicUsize::new(0);
        let payload =
            format!("{CONTRACT_JSON_BEGIN}\n{{\"thread_test\":true}}\n{CONTRACT_JSON_END}\n");

        let build = |_: &Path| -> Result<()> {
            build_count.fetch_add(1, Ordering::SeqCst);
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

        let err = load_contract_stdout_for(
            &proofs_dir,
            &OnceLock::new(),
            &flight,
            &|_| Err(io::Error::other("intentional protected failure").into()),
            &|_| unreachable!("generate must not run after build fails"),
        )
        .unwrap_err();
        let err = chain(&err);
        assert!(
            err.contains("intentional protected failure"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains(lock_path.to_string_lossy().as_ref())
                || err.contains("while holding exclusive Lean contract load lock"),
            "failure should retain lock diagnostics, got: {err}"
        );

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

        load_contract_stdout_for(
            &dir_b,
            &OnceLock::new(),
            &Mutex::new(()),
            &|_| Ok(()),
            &|_| Ok(String::from("b")),
        )
        .unwrap();

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
                Err(LeanContractError::BuildFailed {
                    cwd: dir.to_path_buf(),
                    failures: "  status: exit 1\n  stderr:\nbogus".into(),
                })
            },
            &|_| unreachable!("generate must not run after build fails"),
        )
        .unwrap_err();
        let err = chain(&err);

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
    #[expect(
        clippy::disallowed_methods,
        reason = "separate processes hold the file lock for real time"
    )]
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

            let build = |_: &Path| -> Result<()> {
                if holder_path.exists() {
                    std::fs::write(&overlap_path, b"overlap detected").ok();
                    return Err(io::Error::other(
                        "overlapping critical section: holder already present",
                    )
                    .into());
                }
                std::fs::write(&holder_path, std::process::id().to_string())?;
                thread::sleep(Duration::from_millis(HOLD_MS));
                let held_by = std::fs::read_to_string(&holder_path)?;
                if held_by.trim() != std::process::id().to_string() {
                    std::fs::write(&overlap_path, b"holder stolen").ok();
                    return Err(io::Error::other(format!(
                        "holder rewritten by another process: {held_by}"
                    ))
                    .into());
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
    #[expect(
        clippy::disallowed_methods,
        reason = "OS threads hold the lock for real time"
    )]
    fn concurrent_threads_under_lock_never_overlap_mutation() {
        let proofs_dir = unique_temp_dir("thread-overlap");
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));

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
