//! `ExecutableStaging` (`tla/ExecutableStaging.tla`) with the model's bad
//! ordering forced: a sibling thread's spawn forks while the staged
//! executable is being written and stays short of its exec until the staged
//! file has run.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::{Command, Stdio};

use gents_store::test_support::hold_sibling;

/// Watches `directory` for created entries; read the result to wait for one.
fn watch_creation(directory: &Path) -> std::fs::File {
    let directory = std::ffi::CString::new(directory.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: a plain syscall; the descriptor is owned below.
    let inotify = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    assert!(inotify >= 0, "{}", std::io::Error::last_os_error());
    // SAFETY: `inotify` was just returned by inotify_init1 and is owned once.
    let inotify = unsafe { OwnedFd::from_raw_fd(inotify) };
    // SAFETY: `directory` is a NUL-terminated path.
    let watch = unsafe {
        libc::inotify_add_watch(inotify.as_raw_fd(), directory.as_ptr(), libc::IN_CREATE)
    };
    assert!(watch >= 0, "{}", std::io::Error::last_os_error());
    std::fs::File::from(inotify)
}

/// Stages `copy`'s output with the model's bad order forced: the source is a
/// FIFO, so the copy holds the staged file open until its content arrives, and
/// a held sibling forks exactly then and stays short of its exec until the
/// staged file has run. The content comes from a child process told to write
/// only after the sibling has forked: a FIFO writer held in this process would
/// be inherited by the sibling and keep the copy from ever reading its end.
fn stage_beside_a_sibling_forked_mid_copy(
    copy: fn(&Path, &Path) -> anyhow::Result<()>,
) -> std::io::Result<std::process::Output> {
    let source_directory = tempfile::tempdir().unwrap();
    let target_directory = tempfile::tempdir().unwrap();
    let source = source_directory.path().join("candidate");
    let target = target_directory.path().join("gents");
    let fifo = std::ffi::CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o700) }, 0);

    let mut created = watch_creation(target_directory.path());
    let copying = {
        let (source, target) = (source.clone(), target.clone());
        std::thread::spawn(move || copy(&source, &target))
    };
    let mut feeder = Command::new("/bin/sh")
        .args([
            "-c",
            r#"exec 3>"$1"; read _; printf '#!/bin/sh\nprintf staged\n' >&3"#,
            "sh",
        ])
        .arg(&source)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let event = created.read(&mut [0u8; 4096]).unwrap();
    assert!(event > 0, "an inotify read returns whole events");
    let sibling = hold_sibling();
    feeder.stdin.take().unwrap().write_all(b"go\n").unwrap();
    assert!(feeder.wait().unwrap().success());
    copying.join().unwrap().unwrap();
    let ran = Command::new(&target).output();
    sibling.release();
    ran
}

#[test]
fn a_sibling_forked_mid_copy_does_not_keep_the_staged_executable_busy() {
    let ran = stage_beside_a_sibling_forked_mid_copy(super::files::copy_executable);
    assert_eq!(ran.unwrap().stdout, b"staged");
}
