//! `ExecutableStaging` (`tla/ExecutableStaging.tla`) with the model's bad
//! ordering forced: a sibling thread's spawn forks while the staged
//! executable is being written and stays short of its exec until the staged
//! file has run.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::process::Command;

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

/// The source is a FIFO, so the copy holds the staged file open while it
/// waits for content: the test forks a held sibling exactly then, and execs
/// the staged file before that sibling execs.
#[test]
fn a_sibling_forked_mid_copy_does_not_keep_the_staged_executable_busy() {
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
        std::thread::spawn(move || super::files::copy_executable(&source, &target))
    };
    let mut content = std::fs::OpenOptions::new()
        .write(true)
        .open(&source)
        .unwrap();
    created.read(&mut [0u8; 4096]).unwrap();
    let sibling = hold_sibling();
    content.write_all(b"#!/bin/sh\nprintf staged\n").unwrap();
    drop(content);
    copying.join().unwrap().unwrap();
    let ran = Command::new(&target).output();
    sibling.release();
    assert_eq!(ran.unwrap().stdout, b"staged");
}
