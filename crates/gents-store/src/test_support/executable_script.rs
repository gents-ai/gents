use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};

/// Write `contents` to `path` as an executable (mode 755) that the calling
/// test may then exec.
///
/// The file is written by a `/bin/sh` child, through its `printf` builtin,
/// and this call returns only after reaping it; the test process never opens
/// the file for writing. Linux execve refuses a file any process holds open
/// for writing (ETXTBSY), and a sibling test's spawn copies this process's
/// descriptors into its child until that child execs, so a write descriptor
/// held here, even with O_CLOEXEC, can outlive the write in another process.
/// `tla/ScriptFixtureExec.tla` (`ChildWriter`) checks the order; a script
/// whose contents are static belongs in the repository instead.
pub fn write_executable_script(path: &Path, contents: &str) -> std::io::Result<()> {
    let status = Command::new("/bin/sh")
        .args(["-c", r#"printf '%s' "$2" > "$1""#, "sh"])
        .arg(path)
        .arg(contents)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "writing {} exited with {status}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
}

/// Copy the executable `source` to `target`, keeping its mode, for the
/// calling test to exec. The copy is made by a `/bin/cp` child this call
/// reaps, for the reason [`write_executable_script`] gives.
pub fn copy_executable(source: &Path, target: &Path) -> std::io::Result<()> {
    let status = Command::new("/bin/cp")
        .arg("--")
        .arg(source)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "copying {} to {} exited with {status}",
            source.display(),
            target.display()
        )))
    }
}

/// A sibling thread's spawn stopped between its fork and its exec, the
/// window in which its child holds a copy of every descriptor this
/// process had open at the fork. [`HeldSibling::release`] lets it exec.
pub struct HeldSibling {
    release: std::io::PipeWriter,
    spawned: std::thread::JoinHandle<std::io::Result<Child>>,
}

pub fn hold_sibling() -> HeldSibling {
    let (mut forked_reader, forked_writer) = std::io::pipe().unwrap();
    let (release_reader, release) = std::io::pipe().unwrap();
    let (forked, released, releaser) = (
        forked_writer.as_raw_fd(),
        release_reader.as_raw_fd(),
        release.as_raw_fd(),
    );
    // `true` from PATH: macOS has no /bin/true.
    let mut command = Command::new("true");
    // SAFETY: the hook runs in the forked child before exec and calls only
    // the async-signal-safe `close`, `write` and `read` on descriptors this
    // closure's spawning thread keeps open until the spawn returns. The child
    // first closes its own copy of the release pipe's write end: otherwise
    // that copy keeps the pipe open, so a sibling never released (a test that
    // fails before `release`) blocks in `read` forever and holds the test
    // process's output open after the process exits. Closed, the read ends
    // when the parent writes or drops `release`.
    unsafe {
        command.pre_exec(move || {
            libc::close(releaser);
            libc::write(forked, [1u8].as_ptr().cast(), 1);
            libc::read(released, [0u8].as_mut_ptr().cast(), 1);
            Ok(())
        });
    }
    let spawned = std::thread::spawn(move || {
        let _open = (forked_writer, release_reader);
        command.spawn()
    });
    forked_reader.read_exact(&mut [0u8]).unwrap();
    HeldSibling { release, spawned }
}

impl HeldSibling {
    pub fn release(mut self) {
        self.release.write_all(&[1]).unwrap();
        let mut child = self.spawned.join().unwrap().unwrap();
        assert!(child.wait().unwrap().success());
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::{hold_sibling, write_executable_script};

    /// Siblings forked before and after the write, and still short of their
    /// exec, hold nothing that keeps the script from executing.
    #[test]
    fn siblings_held_before_exec_leave_the_written_script_executable() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("script");
        let before = hold_sibling();
        write_executable_script(&script, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        let after = hold_sibling();
        let output = Command::new(&script).arg("ran").output();
        after.release();
        before.release();
        assert_eq!(output.unwrap().stdout, b"ran");
    }

    #[test]
    fn siblings_held_before_exec_leave_the_copied_executable_executable() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let copy = directory.path().join("copy");
        write_executable_script(&source, "#!/bin/sh\nprintf '%s' \"$1\"\n").unwrap();
        let before = hold_sibling();
        super::copy_executable(&source, &copy).unwrap();
        let after = hold_sibling();
        let output = Command::new(&copy).arg("copied").output();
        after.release();
        before.release();
        assert_eq!(output.unwrap().stdout, b"copied");
    }
}
