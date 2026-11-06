//! Native child-process construction shared by every production spawn site.
//!
//! Darwin's posix_spawn file actions return EBADF for a dup2 source descriptor
//! at or above OPEN_MAX (10240). The CLI raises RLIMIT_NOFILE above that bound
//! for embedded DefraDB, so piped or null stdio on a high descriptor number
//! fails before exec. Rust takes posix_spawn only when no pre-exec hook is set,
//! so a no-op hook selects std's fork+exec path, which has no such bound.

use std::ffi::OsStr;
use std::io;

/// Builds a std command whose spawn cannot hit the posix_spawn descriptor bound
/// on macOS. Use this where no managed-capture owner applies; the managed owner
/// keeps posix_spawn and retries on the fork path only after EBADF.
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    disable_posix_spawn(&mut command);
    command
}

/// Spawns a tokio command, retrying once on std's fork+exec path when macOS
/// posix_spawn rejects a dup2 descriptor at or above OPEN_MAX with EBADF.
/// Posix_spawn creates no child on failure, so the retry cannot duplicate one.
/// The fork path lets a child retain unrelated pipe writers until it execs,
/// which is why posix_spawn stays the first attempt.
pub(crate) fn spawn_tokio(
    command: &mut tokio::process::Command,
) -> io::Result<tokio::process::Child> {
    match command.spawn() {
        #[cfg(target_os = "macos")]
        Err(error) if error.raw_os_error() == Some(libc::EBADF) => {
            disable_posix_spawn(command.as_std_mut());
            command.spawn()
        }
        result => result,
    }
}

/// Selects std's fork+exec path. The hook performs no work, so it is safe in the
/// forked child before exec.
#[cfg(target_os = "macos")]
pub(crate) fn disable_posix_spawn(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the closure is empty and does not allocate, lock or touch
    // descriptors in the forked child.
    unsafe {
        command.pre_exec(|| Ok(()));
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn disable_posix_spawn(_command: &mut std::process::Command) {}
