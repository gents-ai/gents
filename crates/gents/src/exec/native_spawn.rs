//! Native child-process spawning shared by every production spawn site.
//!
//! Darwin's posix_spawn file actions return EBADF for a dup2 source descriptor
//! at or above OPEN_MAX (10240). The CLI raises RLIMIT_NOFILE above that bound
//! for embedded DefraDB, so piped or null stdio on a high descriptor number
//! fails before exec. Rust takes posix_spawn only when no pre-exec hook is set,
//! so a no-op hook selects std's fork+exec path, which has no such bound.
//!
//! Fork is not the default: in a heavily threaded process with a large heap,
//! fork stalls in macOS malloc's fork handlers (`_malloc_fork_prepare` takes
//! every zone lock while other threads hold or wait on them), and the forked
//! child retains unrelated pipe writers until it execs. Every spawn therefore
//! tries posix_spawn first and takes the fork path only after EBADF.
//! Posix_spawn creates no child when it fails, so the retry cannot duplicate
//! one. Std's and tokio's `output` and `status` return that spawn error before
//! any child exists; waiting reports ECHILD or EINTR, never EBADF.
//!
//! The policy is modelled by `Proofs/NativeSpawn.lean`; its generated cases
//! drive [`spawn_with_fork_fallback`] in this module's tests.

use std::io;
use std::process::{Child, ExitStatus, Output};

/// Spawns a std command: posix_spawn first, fork+exec once after EBADF.
pub fn spawn_std(command: &mut std::process::Command) -> io::Result<Child> {
    spawn_with_fork_fallback(
        command,
        std::process::Command::spawn,
        disable_posix_spawn,
        HOST_PLATFORM,
    )
}

/// Runs a std command to completion with `Command::output` stdio defaults,
/// under the same posix_spawn-first policy as [`spawn_std`].
pub fn output_std(command: &mut std::process::Command) -> io::Result<Output> {
    spawn_with_fork_fallback(
        command,
        std::process::Command::output,
        disable_posix_spawn,
        HOST_PLATFORM,
    )
}

/// Runs a std command to completion with `Command::status` stdio defaults,
/// under the same posix_spawn-first policy as [`spawn_std`].
pub fn status_std(command: &mut std::process::Command) -> io::Result<ExitStatus> {
    spawn_with_fork_fallback(
        command,
        std::process::Command::status,
        disable_posix_spawn,
        HOST_PLATFORM,
    )
}

/// Spawns a tokio command under the same policy as [`spawn_std`].
pub fn spawn_tokio(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    spawn_with_fork_fallback(
        command,
        tokio::process::Command::spawn,
        |command| disable_posix_spawn(command.as_std_mut()),
        HOST_PLATFORM,
    )
}

/// Runs a tokio command to completion with `Command::output` stdio defaults,
/// under the same policy as [`spawn_std`].
pub async fn output_tokio(command: &mut tokio::process::Command) -> io::Result<Output> {
    match command.output().await {
        Err(error) if fork_fallback_applies(HOST_PLATFORM, &error) => {
            disable_posix_spawn(command.as_std_mut());
            command.output().await
        }
        result => result,
    }
}

/// Platform class of `NativeSpawn.Platform`: only macOS has the posix_spawn
/// descriptor bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpawnPlatform {
    MacOs,
    Other,
}

const HOST_PLATFORM: SpawnPlatform = if cfg!(target_os = "macos") {
    SpawnPlatform::MacOs
} else {
    SpawnPlatform::Other
};

/// `NativeSpawn.forkFallbackApplies`: only EBADF on macOS selects the fork path.
fn fork_fallback_applies(platform: SpawnPlatform, error: &io::Error) -> bool {
    platform == SpawnPlatform::MacOs && error.raw_os_error() == Some(libc::EBADF)
}

/// `NativeSpawn.run`: one posix_spawn attempt, then at most one fork+exec
/// attempt when [`fork_fallback_applies`].
fn spawn_with_fork_fallback<C, T, A, F>(
    command: &mut C,
    mut attempt: A,
    fork_path: F,
    platform: SpawnPlatform,
) -> io::Result<T>
where
    A: FnMut(&mut C) -> io::Result<T>,
    F: FnOnce(&mut C),
{
    match attempt(command) {
        Err(error) if fork_fallback_applies(platform, &error) => {
            fork_path(command);
            attempt(command)
        }
        result => result,
    }
}

/// Selects std's fork+exec path. The hook performs no work, so it is safe in the
/// forked child before exec.
#[cfg(target_os = "macos")]
fn disable_posix_spawn(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure is empty and does not allocate, lock or touch
    // descriptors in the forked child.
    unsafe {
        command.pre_exec(|| Ok(()));
    }
}

#[cfg(not(target_os = "macos"))]
fn disable_posix_spawn(_command: &mut std::process::Command) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::sync::OnceLock;

    #[derive(Debug, serde::Deserialize)]
    struct SpawnCase {
        platform: String,
        first: String,
        retry: String,
        paths: Vec<String>,
        result: String,
    }

    fn model_cases() -> &'static [SpawnCase] {
        static CASES: OnceLock<Vec<SpawnCase>> = OnceLock::new();
        CASES.get_or_init(|| {
            let stdout = gents_lean_contract::run_prepared_generator(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs"),
                "Proofs.Conformance.NativeSpawn:olean",
                "Proofs/Conformance/NativeSpawn.lean",
            )
            .expect("generate native spawn cases");
            serde_json::from_str(stdout.trim()).expect("parse native spawn cases")
        })
    }

    fn platform(name: &str) -> SpawnPlatform {
        match name {
            "macos" => SpawnPlatform::MacOs,
            "other" => SpawnPlatform::Other,
            other => panic!("unknown model platform {other}"),
        }
    }

    /// Realizes one model outcome: `ok` really runs the command on the path
    /// selected so far; errors are injected with their OS codes.
    fn realize(outcome: &str, command: &mut std::process::Command) -> io::Result<()> {
        match outcome {
            "ok" => {
                let status = command.status()?;
                assert!(status.success(), "{status}");
                Ok(())
            }
            "ebadf" => Err(io::Error::from_raw_os_error(libc::EBADF)),
            "other" => Err(io::Error::from_raw_os_error(libc::ENOENT)),
            other => panic!("unknown model outcome {other}"),
        }
    }

    fn outcome_name(result: &io::Result<()>) -> &'static str {
        match result {
            Ok(()) => "ok",
            Err(error) if error.raw_os_error() == Some(libc::EBADF) => "ebadf",
            Err(_) => "other",
        }
    }

    /// Drives `spawn_with_fork_fallback` with the case's environment outcomes
    /// and checks the observed paths and caller result against the model.
    fn drive(case: &SpawnCase) {
        let mut command = std::process::Command::new("/usr/bin/true");
        let forked = Cell::new(false);
        let paths = RefCell::new(Vec::new());
        let result = spawn_with_fork_fallback(
            &mut command,
            |command| {
                let on_fork_path = forked.get();
                paths.borrow_mut().push(if on_fork_path {
                    "fork_exec"
                } else {
                    "posix_spawn"
                });
                realize(
                    if on_fork_path {
                        &case.retry
                    } else {
                        &case.first
                    },
                    command,
                )
            },
            |command| {
                assert!(!forked.replace(true), "fork path selected twice");
                disable_posix_spawn(command);
            },
            platform(&case.platform),
        );
        assert_eq!(*paths.borrow(), case.paths, "{case:?}");
        assert_eq!(outcome_name(&result), case.result, "{case:?}");
    }

    fn drive_selected(select: impl Fn(&SpawnCase) -> bool) {
        let selected = model_cases()
            .iter()
            .filter(|case| select(case))
            .collect::<Vec<_>>();
        assert!(
            !selected.is_empty(),
            "no generated native spawn cases selected"
        );
        for case in selected {
            drive(case);
        }
    }

    fn descriptor_bound_case(case: &SpawnCase) -> bool {
        case.platform == "macos" && case.first == "ebadf"
    }

    #[test]
    fn default_spawn_keeps_posix_spawn() {
        drive_selected(|case| case.first == "ok");
    }

    #[test]
    fn descriptor_bound_retries_once_on_fork_path() {
        drive_selected(descriptor_bound_case);
    }

    #[test]
    fn other_spawn_errors_are_not_retried() {
        drive_selected(|case| case.first != "ok" && !descriptor_bound_case(case));
    }

    #[test]
    fn production_decision_uses_the_host_platform() {
        let ebadf = io::Error::from_raw_os_error(libc::EBADF);
        assert_eq!(
            fork_fallback_applies(HOST_PLATFORM, &ebadf),
            cfg!(target_os = "macos")
        );
    }
}
