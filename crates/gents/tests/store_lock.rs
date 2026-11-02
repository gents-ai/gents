use std::fs;
use std::sync::{Mutex, MutexGuard};

use gents::store::local::home::{default_data_dir, lock_home_store, lock_store};

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

#[test]
fn a_held_store_lock_is_classified_with_its_home_and_holder() {
    let _guard = exclusive();
    let home = tempfile::tempdir().unwrap();
    let data = home.path().join("data");
    std::fs::create_dir_all(&data).unwrap();

    let _held = gents::store::local::home::lock_store(home.path(), &data).unwrap();
    let error = gents::store::local::home::lock_store(home.path(), &data).unwrap_err();

    let held = error
        .lock_held()
        .expect("a contended store lock is classified, not just described");
    assert_eq!(held.home, home.path());
    assert_eq!(held.holder_pid, Some(std::process::id()));
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

#[cfg(unix)]
struct BlockedChild {
    pid: libc::pid_t,
    release: libc::c_int,
}

#[cfg(unix)]
impl BlockedChild {
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

#[test]
fn a_store_outside_the_home_does_not_exclude_the_home_default_store() {
    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join(".gents");
    fs::create_dir_all(&home).unwrap();
    let outside = temp.path().join("elsewhere-data");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(default_data_dir(&home)).unwrap();

    let held = lock_store(&home, &outside).unwrap();
    assert_eq!(
        held.path(),
        fs::canonicalize(temp.path())
            .unwrap()
            .join("elsewhere-data.lock")
    );
    let home_store = lock_store(&home, &default_data_dir(&home))
        .expect("a store outside the home leaves the home store free");
    assert_ne!(held.path(), home_store.path());
}

#[cfg(unix)]
#[test]
fn dropping_the_store_lock_releases_it_even_before_a_forked_child_execs() {
    use std::ffi::CString;

    let _exclusive = exclusive();
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let held = lock_store(temp.path(), &data).unwrap();

    // The child can make only async-signal-safe calls between fork and exec.
    let program = CString::new("/usr/bin/true").unwrap();
    let argv = [program.as_ptr(), std::ptr::null()];
    let mut go = [0; 2];
    assert_eq!(unsafe { libc::pipe(go.as_mut_ptr()) }, 0);
    let pid = unsafe { libc::fork() };
    if pid == 0 {
        unsafe {
            libc::close(go[1]);
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

    let error = lock_store(temp.path(), &data)
        .expect_err("the parent still holds the lock")
        .to_string();
    assert!(
        error.contains(&format!("process {}", std::process::id())),
        "the holder is recorded as this process: {error}"
    );

    drop(held);
    let reacquired = lock_store(temp.path(), &data)
        .expect("the child cannot retain a lock its parent explicitly released");
    let status = child.release();
    assert!(
        libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0,
        "the child exec'd: {status}"
    );
    assert!(
        lock_store(temp.path(), &data).is_err(),
        "child exec must not release the new owner's lock"
    );
    drop(reacquired);
    lock_store(temp.path(), &data).expect("the new owner released its lock");
}
