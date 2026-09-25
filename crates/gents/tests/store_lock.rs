//! Store lock lifetime: exclusion while held, release when dropped.
//!
//! These tests live in their own binary because the store lock is a `flock`
//! on an open file description, and a child forked while it is held shares
//! that description until it execs. The unit binary's tests spawn children
//! through fork (`managed_exec`'s `pre_exec` setsid), so a lock dropped there
//! can stay held by another test's not-yet-exec'd child, and a reacquire
//! after the drop intermittently fails (#1780). Nothing in this binary forks
//! except `a_forked_child_holds_the_store_lock_until_it_execs`, which runs
//! under `FORK_EXCLUSION` with every other test here.

use std::fs;
use std::sync::{Mutex, MutexGuard};

use gents::home::{default_data_dir, lock_home_store, lock_store};

static FORK_EXCLUSION: Mutex<()> = Mutex::new(());

fn exclusive() -> MutexGuard<'static, ()> {
    FORK_EXCLUSION
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn a_second_holder_cannot_lock_a_store() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let data = default_data_dir(&home);
    fs::create_dir_all(&data).unwrap();

    let held = lock_store(&home, &data).expect("the first holder locks the store");
    let error = lock_store(&home, &data)
        .expect_err("a second holder must not open the same store")
        .to_string();
    assert!(error.contains("already using"), "{error}");
    assert!(
        error.contains(&format!("process {}", std::process::id())),
        "{error}"
    );
    assert!(error.contains("gents service stop --home"), "{error}");

    drop(held);
    lock_store(&home, &data).expect("the lock is released with its holder");
}

#[cfg(unix)]
#[test]
fn every_alias_of_a_store_takes_the_same_lock() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let real = temp.path().join("real-data");
    fs::create_dir_all(&real).unwrap();
    let link = temp.path().join("link-data");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let held = lock_store(temp.path(), &real).unwrap();
    assert!(
        lock_store(temp.path(), &link).is_err(),
        "a symlinked data directory is the same store"
    );
    assert!(
        lock_store(temp.path(), &real.join("..").join("real-data")).is_err(),
        "a non-canonical path is the same store"
    );
    drop(held);

    // `--data-dir .` names the current directory, which has a name once
    // resolved.
    let current = lock_store(temp.path(), &real.join(".")).unwrap();
    assert_eq!(
        current.path(),
        fs::canonicalize(temp.path())
            .unwrap()
            .join("real-data.lock")
    );
}

#[test]
fn a_home_lock_without_a_store_excludes_the_store_created_later() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    fs::create_dir_all(&home).unwrap();
    let held = lock_home_store(&home).unwrap();
    assert!(
        !default_data_dir(&home).exists(),
        "locking creates no store"
    );

    fs::create_dir(default_data_dir(&home)).unwrap();
    assert!(
        lock_store(&home, &default_data_dir(&home)).is_err(),
        "an opener that creates the store meets the same lock"
    );
    assert_eq!(
        held.path(),
        fs::canonicalize(&home).unwrap().join("data.lock")
    );
    drop(held);
    lock_store(&home, &default_data_dir(&home)).unwrap();
}

/// A forked child blocked until its release pipe closes. Dropping it
/// closes the pipe and reaps the child, so a failed assertion neither
/// leaves the child blocked nor leaks it.
#[cfg(unix)]
struct BlockedChild {
    pid: libc::pid_t,
    release: libc::c_int,
}

#[cfg(unix)]
impl BlockedChild {
    /// Lets the child exec and returns its wait status.
    fn release(mut self) -> libc::c_int {
        self.release_and_reap()
    }

    fn release_and_reap(&mut self) -> libc::c_int {
        let mut status = 0;
        if self.release >= 0 {
            unsafe { libc::close(self.release) };
            self.release = -1;
            while unsafe { libc::waitpid(self.pid, &mut status, 0) } == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
            }
        }
        status
    }
}

#[cfg(unix)]
impl Drop for BlockedChild {
    fn drop(&mut self) {
        self.release_and_reap();
    }
}

/// The premise the binary split rests on: a child forked while the lock is
/// held keeps the store excluded after the parent drops its `StoreLock`,
/// until the child execs.
#[cfg(unix)]
#[test]
fn a_forked_child_holds_the_store_lock_until_it_execs() {
    use std::ffi::CString;

    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let held = lock_store(temp.path(), &data).unwrap();

    // Everything the child touches is prepared before the fork: between fork
    // and exec it may only make async-signal-safe calls.
    let program = CString::new("/usr/bin/true").unwrap();
    let argv = [program.as_ptr(), std::ptr::null()];
    let mut go = [0; 2];
    assert_eq!(unsafe { libc::pipe(go.as_mut_ptr()) }, 0);
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(go[1]);
            // EOF is the release signal; anything but EINTR ends the wait.
            let mut byte = 0u8;
            while libc::read(go[0], (&mut byte as *mut u8).cast(), 1) == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
            }
            libc::execv(program.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
    }
    unsafe { libc::close(go[0]) };
    if pid < 0 {
        unsafe { libc::close(go[1]) };
        panic!("fork failed: {}", std::io::Error::last_os_error());
    }
    let child = BlockedChild {
        pid,
        release: go[1],
    };

    drop(held);
    let error = lock_store(temp.path(), &data)
        .expect_err("the forked child still holds the dropped lock")
        .to_string();
    assert!(
        error.contains(&format!("process {}", std::process::id())),
        "the holder is recorded as this process: {error}"
    );

    let status = child.release();
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "the child exec'd: {status}"
    );
    lock_store(temp.path(), &data).expect("the lock is released once the child execs");
}
