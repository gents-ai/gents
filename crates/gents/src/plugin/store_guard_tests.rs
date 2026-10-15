//! Tests for the plugin store guard: which home it belongs to, and when the
//! underlying lock is actually held or released.
//!
//! Every assertion here is single-threaded and nonblocking: held-versus-free
//! is probed with [`crate::file_lock::FileLock::try_exclusive`] on a second,
//! independently opened descriptor of the same lock file, mirroring how
//! `file_lock.rs`'s own tests distinguish separate open file descriptions
//! without ever waiting on another holder.

use super::*;

fn open_lock_file(home: &Path) -> std::fs::File {
    let path = plugins_root(home).join(".store.lock");
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .unwrap()
}

fn lock_is_free(home: &Path) -> bool {
    match crate::file_lock::FileLock::try_exclusive(open_lock_file(home)) {
        Ok(probe) => {
            drop(probe);
            true
        }
        Err(std::fs::TryLockError::WouldBlock) => false,
        Err(error) => panic!("probing the store lock failed: {error}"),
    }
}

/// `for_home` accepts only the exact home the guard was taken for: the same
/// tempdir passes, and a different home is refused naming both. The exactness
/// is the point: a guard is proof about one home, and anything else must ask
/// for its own.
#[test]
fn for_home_accepts_only_the_home_the_guard_was_taken_for() {
    let home = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let guard = lock_store(home.path(), false).unwrap();

    guard.for_home(home.path()).expect("the guard's own home");
    guard
        .for_home(other.path())
        .expect_err("a guard for one home must not pass for another");

    let error = guard.for_home(other.path()).unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("belongs to"), "{message}");
    assert!(message.contains(home.path().to_str().unwrap()), "{message}");
    assert!(
        message.contains(other.path().to_str().unwrap()),
        "{message}"
    );
}

/// A shared guard really holds the lock: a second descriptor cannot take it
/// exclusively, and dropping the guard releases it for the next holder
/// without any explicit unlock. Exclusive mode behaves the same. This is the
/// RAII guarantee mutations rely on: leaving a scope leaves nothing locked.
#[test]
fn a_guard_holds_the_lock_until_dropped_then_releases_it() {
    let home = tempfile::tempdir().unwrap();

    let shared = lock_store(home.path(), false).unwrap();
    assert!(!lock_is_free(home.path()), "shared guard holds the lock");
    drop(shared);
    assert!(lock_is_free(home.path()), "dropping a shared guard unlocks");

    let exclusive = lock_store(home.path(), true).unwrap();
    assert!(
        !lock_is_free(home.path()),
        "exclusive guard holds the lock"
    );
    drop(exclusive);
    assert!(
        lock_is_free(home.path()),
        "dropping an exclusive guard unlocks"
    );

    // The next caller can take the lock again immediately.
    lock_store(home.path(), true).unwrap();
}

/// `lock_store` prepares the store's own lock file under the plugins root
/// before locking, so the first install in a fresh home needs nothing else.
#[test]
fn lock_store_creates_the_lock_file_under_the_plugins_root() {
    let home = tempfile::tempdir().unwrap();
    let path = plugins_root(home.path()).join(".store.lock");
    assert!(!path.exists());
    let _guard = lock_store(home.path(), false).unwrap();
    assert!(path.is_file(), "{} must exist", path.display());
}
