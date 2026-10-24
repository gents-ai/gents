use super::*;
use gents_protocol::error_chain::ErrorChain;

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
    let message = format!("{}", ErrorChain(&error));
    assert!(message.contains("belongs to"), "{message}");
    assert!(message.contains(home.path().to_str().unwrap()), "{message}");
    assert!(
        message.contains(other.path().to_str().unwrap()),
        "{message}"
    );
}

#[test]
fn a_guard_holds_the_lock_until_dropped_then_releases_it() {
    let home = tempfile::tempdir().unwrap();

    let shared = lock_store(home.path(), false).unwrap();
    assert!(!lock_is_free(home.path()), "shared guard holds the lock");
    drop(shared);
    assert!(lock_is_free(home.path()), "dropping a shared guard unlocks");

    let exclusive = lock_store(home.path(), true).unwrap();
    assert!(!lock_is_free(home.path()), "exclusive guard holds the lock");
    drop(exclusive);
    assert!(lock_is_free(home.path()));
    lock_store(home.path(), true).unwrap();
}

#[test]
fn lock_store_creates_the_lock_file_under_the_plugins_root() {
    let home = tempfile::tempdir().unwrap();
    let path = plugins_root(home.path()).join(".store.lock");
    assert!(!path.exists());
    let _guard = lock_store(home.path(), false).unwrap();
    assert!(path.is_file(), "{} must exist", path.display());
}

#[test]
fn try_lock_store_reports_contention_without_waiting() {
    let home = tempfile::tempdir().unwrap();
    let _held = lock_store(home.path(), true).unwrap();
    let error = match try_lock_store(home.path()) {
        Ok(_) => panic!("the held lock must be reported busy"),
        Err(error) => error,
    };
    assert!(format!("{}", ErrorChain(&error)).contains("store is busy"));
}
