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

use std::io;
use std::process::{Child, ExitStatus, Output};

/// Spawns a std command: posix_spawn first, fork+exec once after EBADF.
pub fn spawn_std(command: &mut std::process::Command) -> io::Result<Child> {
    spawn_with_fork_fallback(command, std::process::Command::spawn, disable_posix_spawn)
}

/// Runs a std command to completion with `Command::output` stdio defaults,
/// under the same posix_spawn-first policy as [`spawn_std`].
pub fn output_std(command: &mut std::process::Command) -> io::Result<Output> {
    spawn_with_fork_fallback(command, std::process::Command::output, disable_posix_spawn)
}

/// Runs a std command to completion with `Command::status` stdio defaults,
/// under the same posix_spawn-first policy as [`spawn_std`].
pub fn status_std(command: &mut std::process::Command) -> io::Result<ExitStatus> {
    spawn_with_fork_fallback(command, std::process::Command::status, disable_posix_spawn)
}

/// Spawns a tokio command under the same policy as [`spawn_std`].
pub fn spawn_tokio(command: &mut tokio::process::Command) -> io::Result<tokio::process::Child> {
    spawn_with_fork_fallback(command, tokio::process::Command::spawn, |command| {
        disable_posix_spawn(command.as_std_mut())
    })
}

/// Runs a tokio command to completion with `Command::output` stdio defaults,
/// under the same policy as [`spawn_std`].
pub async fn output_tokio(command: &mut tokio::process::Command) -> io::Result<Output> {
    match command.output().await {
        Err(error) if is_posix_spawn_descriptor_bound(&error) => {
            disable_posix_spawn(command.as_std_mut());
            command.output().await
        }
        result => result,
    }
}

fn spawn_with_fork_fallback<C, T, A, F>(
    command: &mut C,
    mut attempt: A,
    fork_path: F,
) -> io::Result<T>
where
    A: FnMut(&mut C) -> io::Result<T>,
    F: FnOnce(&mut C),
{
    match attempt(command) {
        Err(error) if is_posix_spawn_descriptor_bound(&error) => {
            fork_path(command);
            attempt(command)
        }
        result => result,
    }
}

#[cfg(target_os = "macos")]
fn is_posix_spawn_descriptor_bound(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EBADF)
}

#[cfg(not(target_os = "macos"))]
fn is_posix_spawn_descriptor_bound(_error: &io::Error) -> bool {
    false
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

    /// The default spawn is a single posix_spawn attempt: no pre-exec hook is
    /// installed unless that attempt fails with the descriptor bound.
    #[test]
    fn default_spawn_keeps_posix_spawn() {
        let mut command = std::process::Command::new("/usr/bin/true");
        let mut attempts = 0;
        let mut forked = false;
        let status = spawn_with_fork_fallback(
            &mut command,
            |command| {
                attempts += 1;
                command.status()
            },
            |_| forked = true,
        )
        .unwrap();
        assert!(status.success());
        assert_eq!(attempts, 1);
        assert!(!forked, "fork path must not be selected without EBADF");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn descriptor_bound_retries_once_on_fork_path() {
        let mut command = std::process::Command::new("/usr/bin/true");
        let mut attempts = 0;
        let mut forked = 0;
        let status = spawn_with_fork_fallback(
            &mut command,
            |command| {
                attempts += 1;
                if attempts == 1 {
                    Err(io::Error::from_raw_os_error(libc::EBADF))
                } else {
                    command.status()
                }
            },
            |command| {
                forked += 1;
                disable_posix_spawn(command);
            },
        )
        .unwrap();
        assert!(status.success());
        assert_eq!((attempts, forked), (2, 1));
    }

    #[test]
    fn other_spawn_errors_are_not_retried() {
        let mut command = std::process::Command::new("/nonexistent/gents-native-spawn");
        let mut attempts = 0;
        let error = spawn_with_fork_fallback(
            &mut command,
            |command| {
                attempts += 1;
                command.spawn()
            },
            |_| panic!("fork path selected for a non-EBADF error"),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(attempts, 1);
    }
}
