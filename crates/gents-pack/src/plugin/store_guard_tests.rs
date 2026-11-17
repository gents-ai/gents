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
    match gents_store::local::file_lock::FileLock::try_exclusive(open_lock_file(home)) {
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
    bootstrap_store(home.path()).unwrap();
    let guard = try_lock_store_shared(home.path()).unwrap();

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

#[test]
fn a_guard_holds_the_lock_until_dropped_then_releases_it() {
    let home = tempfile::tempdir().unwrap();

    bootstrap_store(home.path()).unwrap();
    let shared = try_lock_store_shared(home.path()).unwrap();
    assert!(!lock_is_free(home.path()), "shared guard holds the lock");
    drop(shared);
    assert!(lock_is_free(home.path()), "dropping a shared guard unlocks");

    let exclusive = lock_store(home.path()).unwrap();
    assert!(!lock_is_free(home.path()), "exclusive guard holds the lock");
    drop(exclusive);
    assert!(lock_is_free(home.path()));
    lock_store(home.path()).unwrap();
}

#[test]
fn try_lock_store_reports_contention_without_waiting() {
    let home = tempfile::tempdir().unwrap();
    let _held = lock_store(home.path()).unwrap();
    let error = match try_lock_store(home.path()) {
        Ok(_) => panic!("the held lock must be reported busy"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("store is busy"));
}

#[tokio::test]
async fn installer_waits_for_a_reconcile_read_lease_and_is_cancellation_safe() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let reconcile = try_lock_store_shared(home.path()).unwrap();
    let mut waiting = Box::pin(lock_store_until(
        home.path(),
        tokio::time::Instant::now() + std::time::Duration::from_secs(60),
    ));
    assert!(futures::poll!(waiting.as_mut()).is_pending());
    drop(reconcile);
    let installed = waiting.await.unwrap();
    assert!(!lock_is_free(home.path()));
    drop(installed);
    assert!(lock_is_free(home.path()));

    bootstrap_store(home.path()).unwrap();
    let reconcile = try_lock_store_shared(home.path()).unwrap();
    let mut cancelled = Box::pin(lock_store_until(
        home.path(),
        tokio::time::Instant::now() + std::time::Duration::from_secs(60),
    ));
    assert!(futures::poll!(cancelled.as_mut()).is_pending());
    drop(cancelled);
    drop(reconcile);
    assert!(
        lock_is_free(home.path()),
        "no abandoned blocking task may later acquire the writer lease"
    );
}

#[tokio::test]
async fn installer_wait_timeout_preserves_typed_busy() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let reconcile = try_lock_store_shared(home.path()).unwrap();
    let error = lock_store_until(home.path(), tokio::time::Instant::now())
        .await
        .err()
        .expect("busy lease must exhaust its budget");
    assert!(PublishedPluginReadError::error_chain_is_busy(
        error.as_ref()
    ));
    drop(reconcile);
}
