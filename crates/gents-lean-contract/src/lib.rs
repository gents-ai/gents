use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN GENTS LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END GENTS LEAN CONTRACT JSON---";

// Build only the module artifact needed by `lake env lean --run`; the broader
// target can pull package extra artifacts such as ProofWidgets' widget bundle.
const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";
const LAKE_BUILD_ATTEMPTS: usize = 3;

/// Advisory lock file under the proofs `.lake` tree. Scoped per proofs checkout
/// so independent worktrees do not serialize on each other, while every
/// consumer of the same checkout shares one exclusive mutation guard.
const LOCK_FILE_NAME: &str = "gents-lean-contract.lock";

/// Process-wide successful stdout payload. Failures are not cached so the next
/// caller can retry after a transient or prior error.
static PROCESS_STDOUT_CACHE: OnceLock<String> = OnceLock::new();

/// In-process single-flight gate. Only one thread runs Lake / generation; the
/// rest wait and then observe the cache (or retry after a failed attempt).
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
    if let Some(cached) = PROCESS_STDOUT_CACHE.get() {
        return Ok(cached.clone());
    }

    let _in_process = PROCESS_LOAD_MUTEX
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(cached) = PROCESS_STDOUT_CACHE.get() {
        return Ok(cached.clone());
    }

    let proofs_dir = proofs_dir()?;
    let stdout = load_contract_stdout_uncached(&proofs_dir)?;
    let _ = PROCESS_STDOUT_CACHE.set(stdout.clone());
    Ok(stdout)
}

/// Build + generate under the cross-process proofs lock, without the process
/// cache. Used by the public loader after the single-flight gate, and by tests.
fn load_contract_stdout_uncached(proofs_dir: &Path) -> Result<String> {
    with_proofs_dir_lock(proofs_dir, || {
        run_lake_build_unlocked(proofs_dir)?;
        run_contract_generator_unlocked(proofs_dir)
    })
}

/// Run `lake build` for the contract target under the proofs-dir lock.
pub fn run_lake_build(proofs_dir: &Path) -> Result<()> {
    with_proofs_dir_lock(proofs_dir, || run_lake_build_unlocked(proofs_dir))
}

fn run_lake_build_unlocked(proofs_dir: &Path) -> Result<()> {
    let mut failures = Vec::new();

    for attempt in 1..=LAKE_BUILD_ATTEMPTS {
        let output = Command::new("lake")
            .args(["build", CONTRACT_TARGET])
            .current_dir(proofs_dir)
            .output()
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

/// Hold an exclusive advisory lock for `proofs_dir` for the duration of `op`.
///
/// The lock file lives under the canonical proofs checkout's `.lake` directory,
/// is released when the guard is dropped (including on panic/process exit), and
/// is independent across distinct proof checkouts (e.g. separate worktrees).
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
            // Fixtures may not exist yet; fall back to an absolute path so the
            // lock is still scoped to this checkout rather than the process cwd.
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

    /// In-process single-flight + cross-process lock with injectable build/generate.
    fn load_with_injectable_ops(
        cache: &Mutex<Option<String>>,
        proofs_dir: &Path,
        build: &dyn Fn(&Path) -> Result<()>,
        generate: &dyn Fn(&Path) -> Result<String>,
    ) -> Result<String> {
        let mut guard = cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = guard.as_ref() {
            return Ok(cached.clone());
        }

        let stdout = with_proofs_dir_lock(proofs_dir, || {
            build(proofs_dir)?;
            generate(proofs_dir)
        })?;
        *guard = Some(stdout.clone());
        Ok(stdout)
    }

    #[test]
    fn concurrent_threads_single_flight_same_payload() {
        let proofs_dir = unique_temp_dir("single-flight");
        let cache = Mutex::new(None);
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
                    load_with_injectable_ops(&cache, &proofs_dir, &build, &generate).unwrap()
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

        let err = with_proofs_dir_lock(&proofs_dir, || -> Result<()> {
            anyhow::bail!("intentional protected failure")
        })
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

        // Lock must be free immediately for the next caller.
        with_proofs_dir_lock(&proofs_dir, || Ok(())).unwrap();

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

        let guard_a = acquire_exclusive_lock(&lock_a).unwrap();
        // Holding A must not block acquisition of B.
        let guard_b = acquire_exclusive_lock(&lock_b).unwrap();

        // Same directory must contend while A is held.
        let contended = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_a)
            .unwrap();
        match contended.try_lock() {
            Err(TryLockError::WouldBlock) => {}
            other => panic!("expected WouldBlock on same proofs lock, got {other:?}"),
        }

        drop(guard_a);
        drop(guard_b);
        // After release, same path is acquirable again.
        let _reacquired = acquire_exclusive_lock(&lock_a).unwrap();

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

        let err = with_proofs_dir_lock(&base, || Ok(()))
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
        let err = with_proofs_dir_lock(&proofs_dir, || -> Result<String> {
            anyhow::bail!(
                "Lean conformance contract build failed\n  cwd: {}\n  status: exit 1\n  stderr:\nbogus",
                proofs_dir.display()
            )
        })
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

            with_proofs_dir_lock(&proofs_dir, || {
                // If another process is inside the critical section, holder exists.
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
            })
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

        thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in 0..6 {
                let proofs_dir = &proofs_dir;
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                handles.push(scope.spawn(move || {
                    with_proofs_dir_lock(proofs_dir, || {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(now, Ordering::SeqCst);
                        thread::sleep(Duration::from_millis(50));
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(i)
                    })
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
