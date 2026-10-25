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

static PROCESS_STDOUT_CACHE: OnceLock<(PathBuf, String)> = OnceLock::new();

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
    cache: &OnceLock<(PathBuf, String)>,
    flight: &Mutex<()>,
    build: &dyn Fn(&Path) -> Result<()>,
    generate: &dyn Fn(&Path) -> Result<String>,
) -> Result<String> {
    let proofs_dir = canonicalize_proofs_dir(proofs_dir)?;
    if let Some((root, cached)) = cache.get() {
        anyhow::ensure!(
            root == &proofs_dir,
            "Lean contract cache belongs to another proofs directory"
        );
        return Ok(cached.clone());
    }

    let _in_process = flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some((root, cached)) = cache.get() {
        anyhow::ensure!(
            root == &proofs_dir,
            "Lean contract cache belongs to another proofs directory"
        );
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

pub fn run_contract_generator(proofs_dir: &Path) -> Result<String> {
    with_proofs_dir_lock(proofs_dir, || run_contract_generator_unlocked(proofs_dir))
}

/// Preparation and execution share the aggregate loader's cross-process lock:
/// Lake configuration and imported oleans are mutable across direct targets.
pub fn run_prepared_generator(proofs_dir: &Path, target: &str, file: &str) -> Result<String> {
    with_proofs_dir_lock(proofs_dir, || {
        let mut command = Command::new("lake");
        command
            .args(["build", target])
            .current_dir(proofs_dir)
            .stdin(Stdio::null());
        let output = run_with_visible_output(&mut command, io::stderr(), io::stderr())
            .with_context(|| format!("failed to prepare Lean target {target}"))?;
        anyhow::ensure!(
            output.status.success(),
            "Lean target preparation failed\n  target: {target}\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(), output.status,
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr)
        );
        run_generator_unlocked(proofs_dir, file)
    })
}

fn run_contract_generator_unlocked(proofs_dir: &Path) -> Result<String> {
    run_generator_unlocked(proofs_dir, CONTRACT_RUN_FILE)
}

fn run_generator_unlocked(proofs_dir: &Path, file: &str) -> Result<String> {
    let output = Command::new("lake")
        .args(["env", "lean", "--run", file])
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

/// Cargo supplies the consuming package's manifest paths at runtime; direct
/// execution must provide the same explicit anchor, and repository metadata
/// bounds discovery to the selected checkout.
pub fn proofs_dir() -> Result<PathBuf> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .context("missing runtime CARGO_MANIFEST_DIR for Lean contract source")?;
    let manifest_path = std::env::var_os("CARGO_MANIFEST_PATH")
        .map(PathBuf::from)
        .context("missing runtime CARGO_MANIFEST_PATH for Lean contract source")?;
    anyhow::ensure!(
        manifest_dir.is_absolute() && manifest_path.is_absolute(),
        "runtime Cargo manifest paths must be absolute"
    );
    let manifest_dir =
        std::fs::canonicalize(&manifest_dir).context("invalid runtime CARGO_MANIFEST_DIR")?;
    let manifest_path =
        std::fs::canonicalize(&manifest_path).context("invalid runtime CARGO_MANIFEST_PATH")?;
    anyhow::ensure!(
        manifest_path.is_file()
            && manifest_path
                .file_name()
                .is_some_and(|name| name == "Cargo.toml")
            && manifest_path.parent() == Some(manifest_dir.as_path()),
        "runtime Cargo manifest paths disagree"
    );

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
    fn direct_generators_prepare_cold_imports_under_the_shared_lock() -> Result<()> {
        let root = unique_temp_dir("direct-cold");
        let result = (|| {
            std::fs::copy(
                proofs_dir()?.join("lean-toolchain"),
                root.join("lean-toolchain"),
            )?;
            std::fs::write(
                root.join("lakefile.lean"),
                "import Lake\nopen Lake DSL\npackage fixture\nlean_lib Fixture\n",
            )?;
            std::fs::create_dir(root.join("Fixture"))?;
            std::fs::write(
                root.join("Fixture/Input.lean"),
                "def fixtureValue : Nat := 21\n",
            )?;
            for name in ["Receipt", "Voice"] {
                std::fs::write(
                    root.join(format!("Fixture/{name}.lean")),
                    "import Fixture.Input\ndef main : IO Unit := IO.println fixtureValue\n",
                )?;
            }
            std::thread::scope(|scope| -> Result<()> {
                let handles: Vec<_> = ["Receipt", "Voice"]
                    .into_iter()
                    .map(|name| {
                        let root = &root;
                        scope.spawn(move || {
                            run_prepared_generator(
                                root,
                                &format!("Fixture.{name}:olean"),
                                &format!("Fixture/{name}.lean"),
                            )
                        })
                    })
                    .collect();
                for handle in handles {
                    assert_eq!(handle.join().expect("generator thread")?.trim(), "21");
                }
                Ok(())
            })?;
            let failure =
                run_prepared_generator(&root, "Fixture.Missing:olean", "Fixture/Missing.lean")
                    .expect_err("missing target must fail during preparation");
            let message = format!("{failure:#}");
            assert!(message.contains("Fixture.Missing:olean"));
            assert!(message.contains("exclusive Lean contract load lock"));
            assert!(message.contains("stderr:"));
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
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
