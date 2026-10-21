use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::{Child, Command};

#[cfg(test)]
mod tests;

/// Rust's supplied-PATH fallback forks on macOS, retaining unrelated writers
/// until exec. Resolve each native spawn attempt without that fallback, retaining
/// Darwin's execvP search errors and ENOEXEC shell handling.
pub(super) fn spawn_child(
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: Option<&HashMap<String, String>>,
    piped_stdin: bool,
) -> io::Result<Child> {
    let spawn = |executable: &OsStr, script: Option<&OsStr>| {
        let mut command = Command::new(executable);
        if let Some(script) = script {
            command.arg(script);
        }
        command
            .args(args)
            .current_dir(cwd)
            .stdin(if piped_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        command.as_std_mut().arg0(program);
        if let Some(environment) = environment {
            command.env_clear().envs(environment);
        }
        command.spawn()
    };
    #[cfg(target_os = "macos")]
    if let Some(result) = spawn_supplied_path(program, cwd, environment, &spawn) {
        return result;
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(path) = spawn_path(program, cwd, environment) {
        if let Ok(child) = spawn(path.as_os_str(), None) {
            return Ok(child);
        }
    }
    spawn(OsStr::new(program), None)
}

/// Darwin execvP continues failed PATH candidates, remembers a reachable
/// EACCES, and invokes /bin/sh on ENOEXEC. Its rules are owned by libc:
/// https://github.com/apple-oss-distributions/Libc/blob/main/gen/FreeBSD/exec.c
#[cfg(target_os = "macos")]
fn spawn_supplied_path(
    program: &str,
    cwd: &Path,
    environment: Option<&HashMap<String, String>>,
    mut spawn: impl FnMut(&OsStr, Option<&OsStr>) -> io::Result<Child>,
) -> Option<io::Result<Child>> {
    if program.is_empty() || program.contains('/') || cwd.as_os_str().is_empty() {
        return None;
    }
    let path = environment?.get("PATH")?;
    let absolute_cwd = match std::path::absolute(cwd) {
        Ok(cwd) => cwd,
        Err(error) => return Some(Err(error)),
    };
    if let Err(error) = std::fs::metadata(absolute_cwd.join(".")) {
        return Some(Err(error));
    }
    let mut denied = false;
    for directory in std::env::split_paths(path) {
        if directory
            .as_os_str()
            .len()
            .saturating_add(program.len())
            .saturating_add(2)
            > libc::PATH_MAX as usize
        {
            continue;
        }
        let relative = if directory.as_os_str().is_empty() {
            Path::new(".").join(program)
        } else {
            directory.join(program)
        };
        let candidate = absolute_cwd.join(&relative);
        let error = match spawn(candidate.as_os_str(), None) {
            Ok(child) => return Some(Ok(child)),
            Err(error) => error,
        };
        match error.raw_os_error() {
            None => return Some(Err(error)),
            Some(libc::ENOENT | libc::ENOTDIR | libc::ELOOP | libc::ENAMETOOLONG) => {}
            Some(libc::ENOEXEC) => {
                return Some(spawn(OsStr::new("/bin/sh"), Some(relative.as_os_str())))
            }
            Some(libc::E2BIG | libc::ENOMEM | libc::ETXTBSY) => return Some(Err(error)),
            Some(libc::EACCES) => denied |= std::fs::metadata(&candidate).is_ok(),
            _ if std::fs::metadata(&candidate).is_err() => {}
            _ => return Some(Err(error)),
        }
    }
    Some(Err(io::Error::from_raw_os_error(if denied {
        libc::EACCES
    } else {
        libc::ENOENT
    })))
}

#[cfg(not(target_os = "macos"))]
fn spawn_path(
    program: &str,
    cwd: &Path,
    environment: Option<&HashMap<String, String>>,
) -> Option<PathBuf> {
    if program.is_empty() || program.contains('/') {
        return None;
    }
    let path = environment?.get("PATH")?;
    let cwd = std::path::absolute(cwd).ok()?;
    for directory in std::env::split_paths(path) {
        let candidate = cwd.join(directory).join(program);
        match std::fs::metadata(&candidate) {
            Ok(_) => return Some(candidate),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return None,
        }
    }
    None
}
