use super::super::{
    bootstrap_store, lock_store, read_published_record, try_lock_store_shared,
    PublishedPluginReadError,
};
use super::*;

#[test]
fn read_admission_replays_the_lean_owner() {
    gents_lean_vocab::families::plugin_store_home_acquire::replay::<Outcome>(|i| {
        acquire(i.same_home, i.store_present, i.lock_present, i.contended)
    });
}

#[test]
fn a_read_of_a_home_without_a_store_creates_nothing() {
    let root = tempfile::tempdir().unwrap();
    for home in [root.path().to_owned(), root.path().join("absent")] {
        let error = read_published_record(&home, "team", "plugin")
            .err()
            .expect("an absent store is not read");
        assert!(
            matches!(
                error,
                PublishedPluginReadError::Absent(StoreAbsent::Missing)
            ),
            "{error:?}"
        );
        assert!(!home
            .join(gents_store::local::home::PLUGINS_DIR_NAME)
            .exists());
    }
    assert!(!root.path().join("absent").exists());
}

#[test]
fn a_bootstrapped_store_is_read_and_contention_is_busy() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    drop(try_lock_store_shared(home.path()).unwrap());
    let writer = lock_store(home.path()).unwrap();
    assert!(matches!(
        try_lock_store_shared(home.path()),
        Err(PublishedPluginReadError::Busy)
    ));
    drop(writer);
    drop(try_lock_store_shared(home.path()).unwrap());
}

#[test]
fn a_captured_identity_never_reads_a_replaced_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    let identity = StoreReadIdentity::capture(&home).unwrap();
    drop(identity.try_shared().unwrap());
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        identity.try_shared(),
        Err(PublishedPluginReadError::Absent(StoreAbsent::Replaced))
    ));
}

#[test]
fn a_removed_home_reads_as_absent_and_is_not_recreated() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    let identity = StoreReadIdentity::capture(&home).unwrap();
    std::fs::remove_dir_all(&home).unwrap();
    assert!(matches!(
        identity.try_shared(),
        Err(PublishedPluginReadError::Absent(StoreAbsent::Missing))
    ));
    assert!(!home.exists());
}
