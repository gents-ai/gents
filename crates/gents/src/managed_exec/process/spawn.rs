use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

#[cfg(test)]
mod tests;

/// Rust falls back to fork when a bare executable uses a supplied PATH. Resolve
/// its first existing candidate so native spawn can close unrelated pipe writers
/// atomically. Keep `argv[0]`, the complete environment and libc's error/search
/// behavior: a failed direct spawn falls back to the original command.
pub(super) fn spawn_child(
    program: &str,
    args: &[String],
    cwd: &Path,
    environment: Option<&HashMap<String, String>>,
    piped_stdin: bool,
) -> io::Result<Child> {
    let spawn = |executable: &OsStr| {
        let mut command = Command::new(executable);
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
    if let Some(path) = spawn_path(program, cwd, environment) {
        if let Ok(child) = spawn(path.as_os_str()) {
            return Ok(child);
        }
    }
    spawn(OsStr::new(program))
}

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
