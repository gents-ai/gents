//! Unlayered leaf: test-only loader that builds and reads Lean conformance contract snapshots.
//!
//! Executes `Proofs.Conformance.Contracts` under `crates/gents/proofs/Proofs/Conformance/`.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

pub const CONTRACT_JSON_BEGIN: &str = "---BEGIN GENTS LEAN CONTRACT JSON---";
pub const CONTRACT_JSON_END: &str = "---END GENTS LEAN CONTRACT JSON---";
pub const CONTRACT_FILE_ENV: &str = "GENTS_LEAN_CONTRACT_FILE";

const CONTRACT_TARGET: &str = "Proofs.Conformance.Contracts:olean";
const CONTRACT_EXE_TARGET: &str = "contracts";
const CONTRACT_RUN_FILE: &str = "Proofs/Conformance/Contracts.lean";
const LAKE_BUILD_ATTEMPTS: usize = 3;

const LOCK_FILE_NAME: &str = "gents-lean-contract.lock";
const GENERATOR_CACHE_DIR: &str = "gents-generator-output";
const BUILT_MARKER_SUFFIX: &str = "built";
const ROOT_BUILD_CONFIG_FILES: &[&str] = &[
    "lakefile.lean",
    "lakefile.toml",
    "lake-manifest.json",
    "lean-toolchain",
];

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
    if let Some(stdout) = try_read_contract_file_override()? {
        return Ok(stdout);
    }
    load_contract_stdout_with_reader(
        &proofs_dir()?,
        &PROCESS_STDOUT_CACHE,
        &PROCESS_LOAD_MUTEX,
        Some(&try_read_fresh_contract_cache),
        &ensure_contract_built_unlocked,
        &run_built_contract_generator_unlocked,
    )
}

fn try_read_contract_file_override() -> Result<Option<String>> {
    let Some(raw_path) = std::env::var_os(CONTRACT_FILE_ENV) else {
        return Ok(None);
    };
    let trimmed = raw_path.to_string_lossy();
    let path_str = trimmed.trim();
    if path_str.is_empty() {
        return Ok(None);
    }
    read_contract_snapshot_file(Path::new(path_str)).map(Some)
}

fn read_contract_snapshot_file(path: &Path) -> Result<String> {
    let raw = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {CONTRACT_FILE_ENV} snapshot from {}",
            path.display()
        )
    })?;
    if raw.contains(CONTRACT_JSON_BEGIN) {
        let _ = extract_contract_json(&raw)?;
        return Ok(raw);
    }
    let trimmed = raw.trim();
    anyhow::ensure!(
        !trimmed.is_empty(),
        "{CONTRACT_FILE_ENV} snapshot {} was empty",
        path.display()
    );
    Ok(format!(
        "{CONTRACT_JSON_BEGIN}\n{trimmed}\n{CONTRACT_JSON_END}\n"
    ))
}

#[cfg(test)]
fn load_contract_stdout_for(
    proofs_dir: &Path,
    cache: &OnceLock<(PathBuf, String)>,
    flight: &Mutex<()>,
    build: &dyn Fn(&Path) -> Result<()>,
    generate: &dyn Fn(&Path) -> Result<String>,
) -> Result<String> {
    load_contract_stdout_with_reader(proofs_dir, cache, flight, None, build, generate)
}

type CachedStdoutReader<'a> = &'a dyn Fn(&Path) -> Result<Option<String>>;
type TargetPreparer<'a> = &'a dyn Fn(&Path, &str) -> Result<()>;
type GeneratorRunner<'a> = &'a dyn Fn(&Path, &str, Option<&str>) -> Result<String>;

fn load_contract_stdout_with_reader(
    proofs_dir: &Path,
    cache: &OnceLock<(PathBuf, String)>,
    flight: &Mutex<()>,
    try_read_cached: Option<CachedStdoutReader<'_>>,
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

    if let Some(try_read) = try_read_cached {
        if let Some(stdout) = with_proofs_dir_shared_lock(&proofs_dir, || try_read(&proofs_dir))? {
            let _ = cache.set((proofs_dir, stdout.clone()));
            return Ok(stdout);
        }
    }

    let stdout = with_proofs_dir_lock(&proofs_dir, || {
        if let Some(try_read) = try_read_cached {
            if let Some(cached) = try_read(&proofs_dir)? {
                return Ok(cached);
            }
        }
        build(&proofs_dir)?;
        generate(&proofs_dir)
    })?;
    let _ = cache.set((proofs_dir, stdout.clone()));
    Ok(stdout)
}

pub fn run_lake_build(proofs_dir: &Path) -> Result<()> {
    run_lake_build_with(proofs_dir, &run_lake_build_unlocked)
}

fn contract_exe_path(proofs_dir: &Path) -> PathBuf {
    let name = if cfg!(windows) {
        "contracts.exe"
    } else {
        CONTRACT_EXE_TARGET
    };
    proofs_dir.join(".lake/build/bin").join(name)
}

#[cfg(target_os = "macos")]
fn configure_lake_exe_env(command: &mut Command, proofs_dir: &Path) {
    // Lean 4.18's bundled LLVM 15 ld64.lld omits SG_READ_ONLY on __DATA_CONST,
    // which macOS 26+ dyld rejects. Linking with Apple's /usr/bin/clang fixes it.
    if std::env::var_os("LEAN_CC").is_some() || !Path::new("/usr/bin/clang").is_file() {
        return;
    }
    let Ok(output) = Command::new("lake")
        .args(["env", "lean", "--print-prefix"])
        .current_dir(proofs_dir)
        .stdin(Stdio::null())
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if prefix.is_empty() {
        return;
    }
    let lib_dir = PathBuf::from(prefix).join("lib");
    let library_path = match std::env::var_os("LIBRARY_PATH") {
        Some(existing) if !existing.is_empty() => {
            let mut combined = lib_dir.into_os_string();
            combined.push(":");
            combined.push(existing);
            combined
        }
        _ => lib_dir.into_os_string(),
    };
    command
        .env("LEAN_CC", "/usr/bin/clang")
        .env("LIBRARY_PATH", library_path);
}

#[cfg(not(target_os = "macos"))]
fn configure_lake_exe_env(_command: &mut Command, _proofs_dir: &Path) {}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous build driver outside any runtime; backs off between blocking lake runs"
)]
fn run_lake_build_target_unlocked(
    proofs_dir: &Path,
    target: &str,
    for_native_exe: bool,
) -> Result<()> {
    let mut failures = Vec::new();

    for attempt in 1..=LAKE_BUILD_ATTEMPTS {
        let mut command = Command::new("lake");
        command
            .args(["build", target])
            .current_dir(proofs_dir)
            .stdin(Stdio::null());
        if for_native_exe {
            configure_lake_exe_env(&mut command, proofs_dir);
        }
        let output = run_with_visible_output(&mut command, io::stdout(), io::stderr())
            .with_context(|| {
                format!(
                    "failed to build Lean conformance target {target} in {}",
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
                "Lean conformance target {target} build failed\n  cwd: {}\n{}",
                proofs_dir.display(),
                failures.join("\n\n")
            );
        }

        std::thread::sleep(Duration::from_secs((attempt as u64) * 5));
    }

    unreachable!("lake build retry loop should return or bail")
}

fn run_lake_build_unlocked(proofs_dir: &Path) -> Result<()> {
    if run_lake_build_target_unlocked(proofs_dir, CONTRACT_EXE_TARGET, true).is_ok() {
        return Ok(());
    }
    let _ = writeln!(
        io::stderr(),
        "gents-lean-contract: compiled `{CONTRACT_EXE_TARGET}` build failed; falling back to `{CONTRACT_TARGET}`"
    );
    run_lake_build_target_unlocked(proofs_dir, CONTRACT_TARGET, false)
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
/// warm cache hits run concurrently under a shared lock; misses upgrade to an
/// exclusive lock and double-check before invoking Lake.
pub fn run_prepared_generator(proofs_dir: &Path, target: &str, file: &str) -> Result<String> {
    if target == CONTRACT_TARGET && file == CONTRACT_RUN_FILE {
        if let Some(stdout) = try_read_contract_file_override()? {
            return Ok(stdout);
        }
        return run_prepared_generator_with(
            proofs_dir,
            target,
            file,
            None,
            &prepare_contract_or_target_unlocked,
            &run_contract_or_generator_with_input_unlocked,
        );
    }
    run_prepared_generator_with(
        proofs_dir,
        target,
        file,
        None,
        &prepare_generator_unlocked,
        &run_generator_with_input_unlocked,
    )
}

/// Execute the same Lean owner against the serialized inputs observed by a
/// native conformance test, reusing warm target builds and cached outputs under
/// the proofs-directory lock.
pub fn run_prepared_generator_with_input(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: &str,
) -> Result<String> {
    run_prepared_generator_with(
        proofs_dir,
        target,
        file,
        Some(input),
        &prepare_generator_unlocked,
        &run_generator_with_input_unlocked,
    )
}

struct TargetBuildOutcome {
    sources_hash: Option<String>,
    rebuilt: bool,
}

enum SharedFastPath {
    CacheHit(String),
    GeneratedFromFreshBuild {
        expected_marker: String,
        key: Option<String>,
        output: String,
    },
    NeedsBuild,
}

fn run_prepared_generator_with(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: Option<&str>,
    prepare: TargetPreparer<'_>,
    run_generator: GeneratorRunner<'_>,
) -> Result<String> {
    match with_proofs_dir_shared_lock(proofs_dir, || {
        try_serve_from_shared_lock(proofs_dir, target, file, input, run_generator)
    })? {
        SharedFastPath::CacheHit(cached) => return Ok(cached),
        SharedFastPath::GeneratedFromFreshBuild {
            expected_marker,
            key,
            output,
        } => {
            if let Some(key) = key {
                with_proofs_dir_lock(proofs_dir, || {
                    let marker_path = target_built_marker_path(proofs_dir, target);
                    if std::fs::read_to_string(&marker_path)
                        .is_ok_and(|recorded| recorded.trim_end_matches('\n') == expected_marker)
                    {
                        let path = generator_cache_path(proofs_dir, target);
                        record_generator_output(&path, &key, &output)?;
                    }
                    Ok(())
                })?;
            }
            return Ok(output);
        }
        SharedFastPath::NeedsBuild => {}
    }
    with_proofs_dir_lock(proofs_dir, || {
        if let Some(cached) = try_read_fresh_generator_cache(proofs_dir, target, file, input)? {
            return Ok(cached);
        }
        let outcome = ensure_target_built_unlocked(proofs_dir, target, |dir| prepare(dir, target))?;
        run_built_generator_with(
            proofs_dir,
            target,
            file,
            input,
            outcome.sources_hash.as_deref(),
            outcome.rebuilt,
            run_generator,
        )
    })
}

fn try_serve_from_shared_lock(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: Option<&str>,
    run_generator: GeneratorRunner<'_>,
) -> Result<SharedFastPath> {
    let Some(module_path) = module_path_for_target(target) else {
        return Ok(SharedFastPath::NeedsBuild);
    };
    let Some((dep_hash, toolchain)) = try_read_target_trace(proofs_dir, &module_path) else {
        return Ok(SharedFastPath::NeedsBuild);
    };
    let is_target_file = Path::new(file) == Path::new(&format!("{module_path}.lean"));
    let cache_path = generator_cache_path(proofs_dir, target);
    let recorded_cache = is_target_file
        .then(|| std::fs::read_to_string(&cache_path).ok())
        .flatten();
    let marker_path = target_built_marker_path(proofs_dir, target);
    let recorded_marker = std::fs::read_to_string(&marker_path).ok();
    if recorded_cache.is_none() && recorded_marker.is_none() {
        return Ok(SharedFastPath::NeedsBuild);
    }

    let sources_hash = compute_sources_hash(proofs_dir)?;
    if let Some(recorded) = recorded_cache {
        if let Some((recorded_key, output)) = recorded.split_once('\n') {
            let expected =
                format_generator_cache_key(target, &dep_hash, &toolchain, &sources_hash, input);
            if recorded_key == expected {
                return Ok(SharedFastPath::CacheHit(output.to_owned()));
            }
        }
    }

    if let Some(recorded) = recorded_marker {
        let expected_marker = format_built_marker_key(target, &dep_hash, &toolchain, &sources_hash);
        if recorded.trim_end_matches('\n') == expected_marker {
            let output = run_generator(proofs_dir, file, input)?;
            let key = is_target_file.then(|| {
                format_generator_cache_key(target, &dep_hash, &toolchain, &sources_hash, input)
            });
            return Ok(SharedFastPath::GeneratedFromFreshBuild {
                expected_marker,
                key,
                output,
            });
        }
    }

    Ok(SharedFastPath::NeedsBuild)
}

fn run_lake_build_with(proofs_dir: &Path, build: &dyn Fn(&Path) -> Result<()>) -> Result<()> {
    if with_proofs_dir_shared_lock(proofs_dir, || {
        is_target_build_fresh(proofs_dir, CONTRACT_TARGET)
    })? {
        return Ok(());
    }
    with_proofs_dir_lock(proofs_dir, || {
        ensure_target_built_unlocked(proofs_dir, CONTRACT_TARGET, build).map(|_| ())
    })
}

fn prepare_generator_unlocked(proofs_dir: &Path, target: &str) -> Result<()> {
    let mut command = Command::new("lake");
    command
        .args(["build", target])
        .current_dir(proofs_dir)
        .stdin(Stdio::null());
    let output = run_with_visible_output(&mut command, io::stderr(), io::stderr())
        .with_context(|| format!("failed to prepare Lean target {target}"))?;
    anyhow::ensure!(output.status.success(),
        "Lean target preparation failed\n  target: {target}\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
        proofs_dir.display(), output.status, String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr));
    Ok(())
}

fn prepare_contract_or_target_unlocked(proofs_dir: &Path, target: &str) -> Result<()> {
    if target == CONTRACT_TARGET {
        return run_lake_build_unlocked(proofs_dir);
    }
    prepare_generator_unlocked(proofs_dir, target)
}

fn run_contract_generator_unlocked(proofs_dir: &Path) -> Result<String> {
    let exe = contract_exe_path(proofs_dir);
    if exe.is_file() {
        match Command::new(&exe)
            .current_dir(proofs_dir)
            .stdin(Stdio::null())
            .output()
        {
            Ok(output) if output.status.success() => {
                if let Ok(stdout) = String::from_utf8(output.stdout) {
                    if extract_contract_json(&stdout).is_ok() {
                        return Ok(stdout);
                    }
                }
                let _ = writeln!(
                    io::stderr(),
                    "gents-lean-contract: compiled `{}` emitted invalid contract output; falling back to `lake env lean --run`",
                    exe.display()
                );
            }
            Ok(output) => {
                let _ = writeln!(
                    io::stderr(),
                    "gents-lean-contract: compiled `{}` exited with {}; falling back to `lake env lean --run`",
                    exe.display(),
                    output.status
                );
            }
            Err(error) => {
                let _ = writeln!(
                    io::stderr(),
                    "gents-lean-contract: failed to spawn `{}` ({error}); falling back to `lake env lean --run`",
                    exe.display()
                );
            }
        }
    }
    run_generator_unlocked(proofs_dir, CONTRACT_RUN_FILE)
}

fn run_contract_or_generator_with_input_unlocked(
    proofs_dir: &Path,
    file: &str,
    input: Option<&str>,
) -> Result<String> {
    if file == CONTRACT_RUN_FILE && input.is_none() {
        return run_contract_generator_unlocked(proofs_dir);
    }
    run_generator_with_input_unlocked(proofs_dir, file, input)
}

fn try_read_fresh_contract_cache(proofs_dir: &Path) -> Result<Option<String>> {
    try_read_fresh_generator_cache(proofs_dir, CONTRACT_TARGET, CONTRACT_RUN_FILE, None)
}

fn ensure_contract_built_unlocked(proofs_dir: &Path) -> Result<()> {
    ensure_target_built_unlocked(proofs_dir, CONTRACT_TARGET, run_lake_build_unlocked).map(|_| ())
}

fn run_built_contract_generator_unlocked(proofs_dir: &Path) -> Result<String> {
    run_built_generator_with(
        proofs_dir,
        CONTRACT_TARGET,
        CONTRACT_RUN_FILE,
        None,
        None,
        false,
        &run_contract_or_generator_with_input_unlocked,
    )
}

fn run_generator_unlocked(proofs_dir: &Path, file: &str) -> Result<String> {
    run_generator_with_input_unlocked(proofs_dir, file, None)
}

/// Runs `file` after the caller ensured `target` is built under the same proofs
/// lock, serving the output recorded for the same build and source fingerprint
/// when there is one.
///
/// A recorded output is reused only when the run file is the target module's
/// own source, `<module>.olean` exists, and Lake's `depHash`, the Lean
/// toolchain, the content hash of all proof sources and build config files, and
/// the generator input all match. A file outside the target module may import
/// modules the target does not, so it always runs.
fn run_built_generator_with(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: Option<&str>,
    precomputed_sources_hash: Option<&str>,
    force_regenerate: bool,
    run_generator: GeneratorRunner<'_>,
) -> Result<String> {
    let Some(key) = generator_cache_key(proofs_dir, target, file, input, precomputed_sources_hash)?
    else {
        return run_generator(proofs_dir, file, input);
    };
    let path = generator_cache_path(proofs_dir, target);
    if !force_regenerate {
        if let Ok(recorded) = std::fs::read_to_string(&path) {
            if let Some((recorded_key, output)) = recorded.split_once('\n') {
                if recorded_key == key {
                    return Ok(output.to_owned());
                }
            }
        }
    }
    let output = run_generator(proofs_dir, file, input)?;
    record_generator_output(&path, &key, &output)?;
    Ok(output)
}

fn ensure_target_built_unlocked<F>(
    proofs_dir: &Path,
    target: &str,
    build: F,
) -> Result<TargetBuildOutcome>
where
    F: FnOnce(&Path) -> Result<()>,
{
    let Some(module_path) = module_path_for_target(target) else {
        build(proofs_dir)?;
        return Ok(TargetBuildOutcome {
            sources_hash: None,
            rebuilt: true,
        });
    };
    let marker_path = target_built_marker_path(proofs_dir, target);
    if let Ok(recorded) = std::fs::read_to_string(&marker_path) {
        if let Some((dep_hash, toolchain)) = try_read_target_trace(proofs_dir, &module_path) {
            let sources_hash = compute_sources_hash(proofs_dir)?;
            let expected = format_built_marker_key(target, &dep_hash, &toolchain, &sources_hash);
            if recorded.trim_end_matches('\n') == expected {
                return Ok(TargetBuildOutcome {
                    sources_hash: Some(sources_hash),
                    rebuilt: false,
                });
            }
        }
    }
    build(proofs_dir)?;
    let (dep_hash, toolchain) = read_target_trace(proofs_dir, &module_path)?;
    let sources_hash = compute_sources_hash(proofs_dir)?;
    let marker = format_built_marker_key(target, &dep_hash, &toolchain, &sources_hash);
    write_atomic_file(&marker_path, &marker)?;
    Ok(TargetBuildOutcome {
        sources_hash: Some(sources_hash),
        rebuilt: true,
    })
}

fn try_read_fresh_generator_cache(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: Option<&str>,
) -> Result<Option<String>> {
    let Some(module_path) = module_path_for_target(target) else {
        return Ok(None);
    };
    if Path::new(file) != Path::new(&format!("{module_path}.lean")) {
        return Ok(None);
    }
    let path = generator_cache_path(proofs_dir, target);
    let Ok(recorded) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Some((recorded_key, output)) = recorded.split_once('\n') else {
        return Ok(None);
    };
    let Some((dep_hash, toolchain)) = try_read_target_trace(proofs_dir, &module_path) else {
        return Ok(None);
    };
    let sources_hash = compute_sources_hash(proofs_dir)?;
    let expected = format_generator_cache_key(target, &dep_hash, &toolchain, &sources_hash, input);
    if recorded_key == expected {
        Ok(Some(output.to_owned()))
    } else {
        Ok(None)
    }
}

fn is_target_build_fresh(proofs_dir: &Path, target: &str) -> Result<bool> {
    let Some(module_path) = module_path_for_target(target) else {
        return Ok(false);
    };
    let marker_path = target_built_marker_path(proofs_dir, target);
    let Ok(recorded) = std::fs::read_to_string(&marker_path) else {
        return Ok(false);
    };
    let Some((dep_hash, toolchain)) = try_read_target_trace(proofs_dir, &module_path) else {
        return Ok(false);
    };
    let sources_hash = compute_sources_hash(proofs_dir)?;
    let expected = format_built_marker_key(target, &dep_hash, &toolchain, &sources_hash);
    Ok(recorded.trim_end_matches('\n') == expected)
}

fn module_path_for_target(target: &str) -> Option<String> {
    target
        .strip_suffix(":olean")
        .map(|module| module.replace('.', "/"))
}

fn olean_path_for_module(proofs_dir: &Path, module_path: &str) -> PathBuf {
    proofs_dir
        .join(".lake/build/lib/lean")
        .join(format!("{module_path}.olean"))
}

fn trace_path_for_module(proofs_dir: &Path, module_path: &str) -> PathBuf {
    proofs_dir
        .join(".lake/build/lib/lean")
        .join(format!("{module_path}.trace"))
}

fn read_target_trace(proofs_dir: &Path, module_path: &str) -> Result<(String, String)> {
    let olean_path = olean_path_for_module(proofs_dir, module_path);
    anyhow::ensure!(
        olean_path.is_file(),
        "expected built Lake olean at {}",
        olean_path.display()
    );
    let trace_path = trace_path_for_module(proofs_dir, module_path);
    let trace = std::fs::read_to_string(&trace_path)
        .with_context(|| format!("reading Lake trace {}", trace_path.display()))?;
    let trace: serde_json::Value = serde_json::from_str(&trace)
        .with_context(|| format!("parsing Lake trace {}", trace_path.display()))?;
    let dep_hash = trace
        .get("depHash")
        .and_then(serde_json::Value::as_str)
        .with_context(|| format!("Lake trace {} has no depHash", trace_path.display()))?
        .to_owned();
    let toolchain = std::fs::read_to_string(proofs_dir.join("lean-toolchain"))
        .context("reading lean-toolchain")?;
    Ok((dep_hash, toolchain))
}

fn try_read_target_trace(proofs_dir: &Path, module_path: &str) -> Option<(String, String)> {
    if !olean_path_for_module(proofs_dir, module_path).is_file() {
        return None;
    }
    let trace = std::fs::read_to_string(trace_path_for_module(proofs_dir, module_path)).ok()?;
    let trace: serde_json::Value = serde_json::from_str(&trace).ok()?;
    let dep_hash = trace
        .get("depHash")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    let toolchain = std::fs::read_to_string(proofs_dir.join("lean-toolchain")).ok()?;
    Some((dep_hash, toolchain))
}

fn format_built_marker_key(
    target: &str,
    dep_hash: &str,
    toolchain: &str,
    sources_hash: &str,
) -> String {
    serde_json::json!({
        "target": target,
        "depHash": dep_hash,
        "toolchain": toolchain,
        "sourcesHash": sources_hash,
    })
    .to_string()
}

fn format_generator_cache_key(
    target: &str,
    dep_hash: &str,
    toolchain: &str,
    sources_hash: &str,
    input: Option<&str>,
) -> String {
    serde_json::json!({
        "target": target,
        "depHash": dep_hash,
        "toolchain": toolchain,
        "sourcesHash": sources_hash,
        "input": input,
    })
    .to_string()
}

fn generator_cache_key(
    proofs_dir: &Path,
    target: &str,
    file: &str,
    input: Option<&str>,
    precomputed_sources_hash: Option<&str>,
) -> Result<Option<String>> {
    let Some(module_path) = module_path_for_target(target) else {
        return Ok(None);
    };
    if Path::new(file) != Path::new(&format!("{module_path}.lean")) {
        return Ok(None);
    }
    let (dep_hash, toolchain) = read_target_trace(proofs_dir, &module_path)?;
    let sources_hash = match precomputed_sources_hash {
        Some(hash) => hash.to_owned(),
        None => compute_sources_hash(proofs_dir)?,
    };
    Ok(Some(format_generator_cache_key(
        target,
        &dep_hash,
        &toolchain,
        &sources_hash,
        input,
    )))
}

fn compute_sources_hash(proofs_dir: &Path) -> Result<String> {
    let mut files = Vec::new();
    collect_source_files(proofs_dir, proofs_dir, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files.dedup_by(|left, right| left.0 == right.0);

    let mut hasher = Sha256::new();
    hasher.update(b"gents-lean-sources-v1\0");
    for (relative, full_path) in files {
        let bytes = std::fs::read(&full_path)
            .with_context(|| format!("reading Lean source {}", full_path.display()))?;
        hasher.update((relative.len() as u64).to_le_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    Ok(encode_hex_lower(&hasher.finalize()))
}

fn collect_source_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let entries = std::fs::read_dir(dir)
        .with_context(|| format!("reading Lean proofs directory {}", dir.display()))?;
    let is_root = dir == root;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let file_type = entry
            .file_type()
            .with_context(|| format!("inspecting {}", entry.path().display()))?;
        if file_type.is_dir() {
            if !should_skip_source_dir(&name) {
                collect_source_files(root, &entry.path(), out)?;
            }
            continue;
        }
        if file_type.is_symlink() && should_skip_source_dir(&name) {
            continue;
        }
        let path = entry.path();
        if (file_type.is_file() || (file_type.is_symlink() && path.is_file()))
            && is_tracked_source_file(is_root, &name)
        {
            let relative = normalized_relative_path(root, &path)?;
            out.push((relative, path));
        }
    }
    Ok(())
}

fn should_skip_source_dir(name: &str) -> bool {
    name.starts_with('.') || name == "target"
}

fn is_tracked_source_file(is_root: bool, name: &str) -> bool {
    name.ends_with(".lean") || (is_root && ROOT_BUILD_CONFIG_FILES.contains(&name))
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path
        .strip_prefix(root)
        .with_context(|| format!("{} is outside {}", path.display(), root.display()))?;
    Ok(relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/"))
}

fn encode_hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn generator_cache_path(proofs_dir: &Path, target: &str) -> PathBuf {
    proofs_dir
        .join(".lake/build")
        .join(GENERATOR_CACHE_DIR)
        .join(target.replace(':', "."))
}

fn target_built_marker_path(proofs_dir: &Path, target: &str) -> PathBuf {
    proofs_dir
        .join(".lake/build")
        .join(GENERATOR_CACHE_DIR)
        .join(format!(
            "{}.{BUILT_MARKER_SUFFIX}",
            target.replace(':', ".")
        ))
}

fn record_generator_output(path: &Path, key: &str, output: &str) -> Result<()> {
    write_atomic_file(path, &format!("{key}\n{output}"))
}

fn write_atomic_file(path: &Path, contents: &str) -> Result<()> {
    let directory = path
        .parent()
        .context("generator cache path has no directory")?;
    std::fs::create_dir_all(directory)
        .with_context(|| format!("creating {}", directory.display()))?;
    let file_name = path
        .file_name()
        .context("generator cache path has no file name")?
        .to_string_lossy();
    let (staged, mut file) = open_unique_staged_file(directory, &file_name)?;
    if let Err(error) = file.write_all(contents.as_bytes()) {
        drop(file);
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("writing {}", staged.display()));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&staged, path) {
        let _ = std::fs::remove_file(&staged);
        return Err(error).with_context(|| format!("recording {}", path.display()));
    }
    Ok(())
}

fn open_unique_staged_file(directory: &Path, file_name: &str) -> Result<(PathBuf, File)> {
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0_u32..1024 {
        let staged = directory.join(format!(".{file_name}.staged.{pid}.{nanos}.{attempt}"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
        {
            Ok(file) => return Ok((staged, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", staged.display()));
            }
        }
    }
    Err(anyhow!(
        "exhausted unique staged cache filenames in {}",
        directory.display()
    ))
}

fn run_generator_with_input_unlocked(
    proofs_dir: &Path,
    file: &str,
    input: Option<&str>,
) -> Result<String> {
    let mut command = Command::new("lake");
    command
        .args(["env", "lean", "--run", file])
        .current_dir(proofs_dir);
    let output = if let Some(input) = input {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| {
                format!(
                    "failed to run Lean conformance contract generator in {}",
                    proofs_dir.display()
                )
            })?;
        let mut stdin = child
            .stdin
            .take()
            .context("Lean generator stdin unavailable")?;
        writeln!(stdin, "{input}").context("writing observed inputs to Lean")?;
        drop(stdin);
        child
            .wait_with_output()
            .context("waiting for Lean conformance generator")?
    } else {
        command.output().with_context(|| {
            format!(
                "failed to run Lean conformance contract generator in {}",
                proofs_dir.display()
            )
        })?
    };
    if !output.status.success() {
        anyhow::bail!(
            "Lean conformance contract generator failed\n  cwd: {}\n  status: {}\n  stdout:\n{}\n  stderr:\n{}",
            proofs_dir.display(), output.status, String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr));
    }
    String::from_utf8(output.stdout).context("Lean conformance contract stdout was not UTF-8")
}

fn with_proofs_dir_shared_lock<T, F>(proofs_dir: &Path, op: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
    let lock_path = lock_path_for(proofs_dir)?;
    let _guard = acquire_shared_lock(&lock_path)?;
    op().with_context(|| {
        format!(
            "while holding shared Lean contract load lock\n  lock: {}\n  proofs: {}",
            lock_path.display(),
            proofs_dir.display()
        )
    })
}

fn with_proofs_dir_lock<T, F>(proofs_dir: &Path, op: F) -> Result<T>
where
    F: FnOnce() -> Result<T>,
{
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

fn open_lock_file(lock_path: &Path) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create Lean contract load lock directory {}",
                parent.display()
            )
        })?;
    }

    OpenOptions::new()
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
        })
}

fn acquire_shared_lock(lock_path: &Path) -> Result<File> {
    let file = open_lock_file(lock_path)?;

    match file.try_lock_shared() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => {
            file.lock_shared().with_context(|| {
                format!(
                    "failed waiting for shared Lean contract load lock\n  lock: {}\n  hint: another process is running lake build / contract generation for this proofs directory",
                    lock_path.display()
                )
            })?;
            Ok(file)
        }
        Err(TryLockError::Error(err)) => Err(err).with_context(|| {
            format!(
                "failed to acquire shared Lean contract load lock at {}",
                lock_path.display()
            )
        }),
    }
}

fn acquire_exclusive_lock(lock_path: &Path) -> Result<File> {
    let file = open_lock_file(lock_path)?;

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
/// bounds discovery to the selected checkout. nextest sets only
/// CARGO_MANIFEST_DIR (remapped to the checkout an archived binary runs
/// against), so the manifest path defaults to that directory's Cargo.toml.
pub fn proofs_dir() -> Result<PathBuf> {
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .context("missing runtime CARGO_MANIFEST_DIR for Lean contract source")?;
    let manifest_path = std::env::var_os("CARGO_MANIFEST_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest_dir.join("Cargo.toml"));
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
    fn generator_output_is_reused_only_for_the_same_module_build() -> Result<()> {
        let root = unique_temp_dir("generator-output");
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
            let input = root.join("Fixture/Input.lean");
            std::fs::write(&input, "def fixtureValue : Nat := 21\n")?;
            std::fs::write(
                root.join("Fixture/Receipt.lean"),
                "import Fixture.Input\ndef main : IO Unit := do\n  let line ← (← IO.getStdin).getLine\n  IO.println s!\"{fixtureValue} {line.trim}\"\n",
            )?;
            let run = |observed: &str| {
                run_prepared_generator_with_input(
                    &root,
                    "Fixture.Receipt:olean",
                    "Fixture/Receipt.lean",
                    observed,
                )
            };
            assert_eq!(run("a")?.trim(), "21 a");

            let recorded = generator_cache_path(&root, "Fixture.Receipt:olean");
            let (key, _) = std::fs::read_to_string(&recorded)?
                .split_once('\n')
                .map(|(key, output)| (key.to_owned(), output.to_owned()))
                .context("recorded generator output")?;
            std::fs::write(&recorded, format!("{key}\nrecorded\n"))?;
            assert_eq!(run("a")?.trim(), "recorded");
            assert_eq!(run("b")?.trim(), "21 b");

            std::fs::write(&recorded, format!("{key}\nrecorded\n"))?;
            std::fs::write(&input, "def fixtureValue : Nat := 22\n")?;
            assert_eq!(run("a")?.trim(), "22 a");
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
    fn concurrent_threads_single_flight_same_payload() {
        let proofs_dir = unique_temp_dir("single-flight");
        let cache = OnceLock::new();
        let flight = Mutex::new(());
        let build_count = AtomicUsize::new(0);
        let generate_count = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(8);
        let payload =
            format!("{CONTRACT_JSON_BEGIN}\n{{\"thread_test\":true}}\n{CONTRACT_JSON_END}\n");

        let build = |_: &Path| -> Result<()> {
            build_count.fetch_add(1, Ordering::SeqCst);
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
                    barrier.wait();
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
    fn concurrent_threads_under_lock_never_overlap_mutation() {
        let proofs_dir = unique_temp_dir("thread-overlap");
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let barrier = std::sync::Barrier::new(6);

        thread::scope(|scope| {
            let mut handles = Vec::new();
            for i in 0..6 {
                let proofs_dir = &proofs_dir;
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                let barrier = &barrier;
                handles.push(scope.spawn(move || {
                    barrier.wait();
                    let build = |_: &Path| -> Result<()> {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        max_active.fetch_max(now, Ordering::SeqCst);
                        thread::yield_now();
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

    fn setup_fixture_project(root: &Path, input_value: &str, with_stdin: bool) -> Result<()> {
        std::fs::copy(
            proofs_dir()?.join("lean-toolchain"),
            root.join("lean-toolchain"),
        )?;
        std::fs::write(
            root.join("lakefile.lean"),
            "import Lake\nopen Lake DSL\npackage fixture\nlean_lib Fixture\n",
        )?;
        std::fs::create_dir_all(root.join("Fixture"))?;
        std::fs::write(
            root.join("Fixture/Input.lean"),
            format!("def fixtureValue : Nat := {input_value}\n"),
        )?;
        let receipt = if with_stdin {
            "import Fixture.Input\ndef main : IO Unit := do\n  let line ← (← IO.getStdin).getLine\n  IO.println s!\"{fixtureValue} {line.trim}\"\n"
        } else {
            "import Fixture.Input\ndef main : IO Unit := IO.println fixtureValue\n"
        };
        std::fs::write(root.join("Fixture/Receipt.lean"), receipt)?;
        Ok(())
    }

    #[test]
    fn shared_lock_fast_path_serves_concurrent_readers_without_invoking_lake() -> Result<()> {
        let root = unique_temp_dir("shared-fast-path");
        let result = (|| {
            setup_fixture_project(&root, "21", false)?;
            assert_eq!(
                run_prepared_generator(&root, "Fixture.Receipt:olean", "Fixture/Receipt.lean")?
                    .trim(),
                "21"
            );

            let barrier = std::sync::Barrier::new(4);
            thread::scope(|scope| -> Result<()> {
                let handles: Vec<_> = (0..4)
                    .map(|_| {
                        let root = &root;
                        let barrier = &barrier;
                        scope.spawn(move || -> Result<String> {
                            with_proofs_dir_shared_lock(root, || {
                                // All four readers must reach the barrier concurrently;
                                // an exclusive lock would deadlock here.
                                barrier.wait();
                                try_read_fresh_generator_cache(
                                    root,
                                    "Fixture.Receipt:olean",
                                    "Fixture/Receipt.lean",
                                    None,
                                )?
                                .context("expected warm cache hit under shared lock")
                            })
                        })
                    })
                    .collect();
                for handle in handles {
                    assert_eq!(handle.join().expect("reader thread")?.trim(), "21");
                }
                Ok(())
            })?;

            let run_barrier = std::sync::Barrier::new(4);
            thread::scope(|scope| -> Result<()> {
                let handles: Vec<_> = (0..4)
                    .map(|_| {
                        let root = &root;
                        let run_barrier = &run_barrier;
                        scope.spawn(move || -> Result<String> {
                            run_barrier.wait();
                            run_prepared_generator_with(
                                root,
                                "Fixture.Receipt:olean",
                                "Fixture/Receipt.lean",
                                None,
                                &|_, _| panic!("lake build must not run on warm cache"),
                                &|_, _, _| panic!("lake env lean must not run on warm cache"),
                            )
                        })
                    })
                    .collect();
                for handle in handles {
                    assert_eq!(handle.join().expect("fast-path thread")?.trim(), "21");
                }
                Ok(())
            })?;

            // Built-marker fast path for run_lake_build skips lake when fresh.
            let contract_dir = root.join(".lake/build/lib/lean/Proofs/Conformance");
            std::fs::create_dir_all(&contract_dir)?;
            std::fs::copy(
                olean_path_for_module(&root, "Fixture/Receipt"),
                olean_path_for_module(&root, "Proofs/Conformance/Contracts"),
            )?;
            std::fs::copy(
                trace_path_for_module(&root, "Fixture/Receipt"),
                trace_path_for_module(&root, "Proofs/Conformance/Contracts"),
            )?;
            let build_calls = AtomicUsize::new(0);
            run_lake_build_with(&root, &|_| {
                build_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })?;
            assert_eq!(build_calls.load(Ordering::SeqCst), 1);
            run_lake_build_with(&root, &|_| {
                panic!("lake build must not run when built marker is fresh")
            })?;
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn cache_invalidates_on_source_config_toolchain_and_missing_olean() -> Result<()> {
        let root = unique_temp_dir("cache-invalidation");
        let result = (|| {
            setup_fixture_project(&root, "21", false)?;
            let target = "Fixture.Receipt:olean";
            let file = "Fixture/Receipt.lean";
            assert_eq!(run_prepared_generator(&root, target, file)?.trim(), "21");

            let cache_path = generator_cache_path(&root, target);
            let write_sentinel = || -> Result<()> {
                let recorded = std::fs::read_to_string(&cache_path)?;
                let (key, _) = recorded.split_once('\n').context("recorded key")?;
                std::fs::write(&cache_path, format!("{key}\nsentinel\n"))?;
                Ok(())
            };

            // Warm hit returns recorded sentinel without invoking lake.
            write_sentinel()?;
            assert_eq!(
                run_prepared_generator_with(
                    &root,
                    target,
                    file,
                    None,
                    &|_, _| panic!("unexpected rebuild on unchanged fixture"),
                    &|_, _, _| panic!("unexpected generator run on unchanged fixture"),
                )?
                .trim(),
                "sentinel"
            );

            // Same-length source edit invalidates both generator cache and built marker.
            std::fs::write(
                root.join("Fixture/Input.lean"),
                "def fixtureValue : Nat := 22\n",
            )?;
            assert!(try_read_fresh_generator_cache(&root, target, file, None)?.is_none());
            assert!(!is_target_build_fresh(&root, target)?);
            assert_eq!(run_prepared_generator(&root, target, file)?.trim(), "22");

            // Editing lakefile.lean invalidates both caches.
            write_sentinel()?;
            let lakefile = root.join("lakefile.lean");
            let mut lakefile_text = std::fs::read_to_string(&lakefile)?;
            lakefile_text.push_str("-- touch\n");
            std::fs::write(&lakefile, lakefile_text)?;
            assert!(try_read_fresh_generator_cache(&root, target, file, None)?.is_none());
            assert!(!is_target_build_fresh(&root, target)?);
            assert_eq!(run_prepared_generator(&root, target, file)?.trim(), "22");

            // Deleting the .olean invalidates both caches and rebuilds it.
            write_sentinel()?;
            let olean = olean_path_for_module(&root, "Fixture/Receipt");
            std::fs::remove_file(&olean)?;
            assert!(try_read_fresh_generator_cache(&root, target, file, None)?.is_none());
            assert!(!is_target_build_fresh(&root, target)?);
            assert_eq!(run_prepared_generator(&root, target, file)?.trim(), "22");
            assert!(olean.is_file());

            // Editing lean-toolchain, lakefile.toml, or lake-manifest.json invalidates sourcesHash.
            let hash_before = compute_sources_hash(&root)?;
            std::fs::write(root.join("lake-manifest.json"), "{}\n")?;
            assert!(try_read_fresh_generator_cache(&root, target, file, None)?.is_none());
            assert!(!is_target_build_fresh(&root, target)?);
            assert_ne!(compute_sources_hash(&root)?, hash_before);

            let hash_with_manifest = compute_sources_hash(&root)?;
            std::fs::write(root.join("lakefile.toml"), "name = \"fixture\"\n")?;
            assert_ne!(compute_sources_hash(&root)?, hash_with_manifest);

            let toolchain_path = root.join("lean-toolchain");
            let original_toolchain = std::fs::read_to_string(&toolchain_path)?;
            std::fs::write(&toolchain_path, format!("{original_toolchain} "))?;
            assert!(try_read_fresh_generator_cache(&root, target, file, None)?.is_none());
            assert!(!is_target_build_fresh(&root, target)?);
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn cold_concurrent_storm_builds_only_once_via_double_check() -> Result<()> {
        let proofs_dir = unique_temp_dir("cold-storm");
        let cache_file = proofs_dir.join(".lake/build/storm-cache.txt");
        let build_count = AtomicUsize::new(0);
        let generate_count = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(8);
        let payload =
            format!("{CONTRACT_JSON_BEGIN}\n{{\"storm_test\":true}}\n{CONTRACT_JSON_END}\n");

        let try_read = |_: &Path| -> Result<Option<String>> {
            match std::fs::read_to_string(&cache_file) {
                Ok(contents) => Ok(Some(contents)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(err) => Err(err.into()),
            }
        };
        let build = |_: &Path| -> Result<()> {
            build_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        let generate = |_: &Path| -> Result<String> {
            generate_count.fetch_add(1, Ordering::SeqCst);
            write_atomic_file(&cache_file, &payload)?;
            Ok(payload.clone())
        };

        thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let proofs_dir = &proofs_dir;
                    let barrier = &barrier;
                    let try_read = &try_read;
                    let build = &build;
                    let generate = &generate;
                    scope.spawn(move || {
                        // Separate OnceLock + Mutex per thread simulates 8 cold nextest processes.
                        barrier.wait();
                        load_contract_stdout_with_reader(
                            proofs_dir,
                            &OnceLock::new(),
                            &Mutex::new(()),
                            Some(try_read),
                            build,
                            generate,
                        )
                        .unwrap()
                    })
                })
                .collect();
            for handle in handles {
                assert_eq!(handle.join().unwrap(), payload);
            }
        });

        assert_eq!(
            build_count.load(Ordering::SeqCst),
            1,
            "cold storm must build only once after double-checking under the exclusive lock"
        );
        assert_eq!(
            generate_count.load(Ordering::SeqCst),
            1,
            "cold storm must generate only once after double-checking under the exclusive lock"
        );
        std::fs::remove_dir_all(&proofs_dir)?;
        Ok(())
    }

    #[test]
    fn different_input_reuses_fresh_target_build_without_rebuilding_lake() -> Result<()> {
        let root = unique_temp_dir("different-input");
        let result = (|| {
            setup_fixture_project(&root, "21", true)?;
            let target = "Fixture.Receipt:olean";
            let file = "Fixture/Receipt.lean";
            assert_eq!(
                run_prepared_generator_with_input(&root, target, file, "a")?.trim(),
                "21 a"
            );
            let barrier = std::sync::Barrier::new(2);
            thread::scope(|scope| -> Result<()> {
                let first = scope.spawn(|| {
                    run_prepared_generator_with(
                        &root,
                        target,
                        file,
                        Some("b"),
                        &|_, _| panic!("lake build must not run when target built marker is fresh"),
                        &|dir, f, inp| {
                            barrier.wait();
                            run_generator_with_input_unlocked(dir, f, inp)
                        },
                    )
                });
                let second = scope.spawn(|| {
                    run_prepared_generator_with(
                        &root,
                        target,
                        file,
                        Some("c"),
                        &|_, _| panic!("lake build must not run when target built marker is fresh"),
                        &|dir, f, inp| {
                            barrier.wait();
                            run_generator_with_input_unlocked(dir, f, inp)
                        },
                    )
                });
                assert_eq!(first.join().expect("first input thread")?.trim(), "21 b");
                assert_eq!(second.join().expect("second input thread")?.trim(), "21 c");
                Ok(())
            })?;
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn contract_snapshot_file_accepts_wrapped_and_raw_json() -> Result<()> {
        let root = unique_temp_dir("snapshot-file");
        let result = (|| {
            let wrapped = root.join("wrapped.txt");
            let payload = format!("{CONTRACT_JSON_BEGIN}\n{{\"ok\":true}}\n{CONTRACT_JSON_END}\n");
            std::fs::write(&wrapped, &payload)?;
            assert_eq!(read_contract_snapshot_file(&wrapped)?, payload);

            let raw = root.join("raw.json");
            std::fs::write(&raw, "{\"ok\":true}\n")?;
            let normalized = read_contract_snapshot_file(&raw)?;
            assert_eq!(extract_contract_json(&normalized)?, "{\"ok\":true}");

            let empty = root.join("empty.json");
            std::fs::write(&empty, "   \n")?;
            assert!(read_contract_snapshot_file(&empty).is_err());
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }

    #[test]
    fn contract_generator_uses_compiled_exe_and_falls_back_when_broken() -> Result<()> {
        let root = unique_temp_dir("compiled-exe");
        let result = (|| {
            std::fs::copy(
                proofs_dir()?.join("lean-toolchain"),
                root.join("lean-toolchain"),
            )?;
            std::fs::write(
                root.join("lakefile.lean"),
                "import Lake\nopen Lake DSL\npackage fixture\nlean_lib Proofs\nlean_exe contracts where\n  root := `Proofs.Conformance.Contracts\n",
            )?;
            std::fs::create_dir_all(root.join("Proofs/Conformance"))?;
            std::fs::write(
                root.join("Proofs/Conformance/Contracts.lean"),
                format!(
                    "def main : IO Unit := do\n  IO.println \"{CONTRACT_JSON_BEGIN}\"\n  IO.println \"{{\\\"source\\\":\\\"lean\\\"}}\"\n  IO.println \"{CONTRACT_JSON_END}\"\n"
                ),
            )?;

            run_lake_build_unlocked(&root)?;
            assert!(
                contract_exe_path(&root).is_file(),
                "expected compiled contracts executable"
            );
            let from_exe = run_contract_generator_unlocked(&root)?;
            assert_eq!(extract_contract_json(&from_exe)?, "{\"source\":\"lean\"}");

            // Replace compiled binary with a failing system executable; loader must fall back to `lake env lean --run`.
            #[cfg(unix)]
            {
                let false_bin = ["/usr/bin/false", "/bin/false"]
                    .into_iter()
                    .map(Path::new)
                    .find(|path| path.is_file())
                    .expect("system false binary");
                let exe = contract_exe_path(&root);
                std::fs::remove_file(&exe)?;
                std::os::unix::fs::symlink(false_bin, &exe)?;
                let from_fallback = run_contract_generator_unlocked(&root)?;
                assert_eq!(
                    extract_contract_json(&from_fallback)?,
                    "{\"source\":\"lean\"}"
                );
            }
            Ok(())
        })();
        std::fs::remove_dir_all(root)?;
        result
    }
}
