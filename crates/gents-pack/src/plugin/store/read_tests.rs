use super::super::write::{write, WriteOutcome, Writer};
use super::super::{
    bootstrap_store, lock_store, read_published_record, try_lock_store, try_lock_store_shared,
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

#[test]
fn a_pin_binds_the_home_it_was_captured_in_even_before_the_store_exists() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let pin = StoreReadPin::capture(&home);
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::Absent(StoreAbsent::Missing))
    ));
    assert!(!pin.is_captured());
    bootstrap_store(&home).unwrap();
    drop(pin.try_shared().unwrap());
    assert!(pin.is_captured());

    let unbound = root.path().join("unbound");
    std::fs::create_dir(&unbound).unwrap();
    let early = StoreReadPin::capture(&unbound);
    std::fs::rename(&unbound, root.path().join("moved")).unwrap();
    std::fs::create_dir(&unbound).unwrap();
    bootstrap_store(&unbound).unwrap();
    assert!(matches!(
        early.try_shared(),
        Err(PublishedPluginReadError::Absent(StoreAbsent::Replaced))
    ));
}

#[test]
fn write_admission_replays_the_lean_owner() {
    gents_lean_vocab::families::plugin_store_home_write::replay::<WriteOutcome, Writer>(|i| {
        write(
            i.writer,
            i.home_present,
            i.same_home,
            i.store_pinned,
            i.store_present,
            i.contended,
        )
    });
}

fn plugins_dir(home: &Path) -> PathBuf {
    home.join(gents_store::local::home::PLUGINS_DIR_NAME)
}

fn absent(result: Result<StoreGuard, PublishedPluginReadError>) -> StoreAbsent {
    match result {
        Err(PublishedPluginReadError::Absent(absent)) => absent,
        Err(error) => panic!("expected an absent store, got {error:?}"),
        Ok(_) => panic!("expected an absent store, got a lease"),
    }
}

#[test]
fn a_runtime_writer_never_creates_a_store_in_a_replaced_or_removed_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let pin = StoreReadPin::capture(&home);
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    std::fs::create_dir(&home).unwrap();
    assert_eq!(absent(pin.try_exclusive()), StoreAbsent::Replaced);
    assert!(!plugins_dir(&home).exists());
    assert!(!plugins_dir(&root.path().join("retired")).exists());

    let removed = root.path().join("removed");
    std::fs::create_dir(&removed).unwrap();
    let pin = StoreReadPin::capture(&removed);
    std::fs::remove_dir(&removed).unwrap();
    assert_eq!(absent(pin.try_exclusive()), StoreAbsent::Missing);
    assert!(!removed.exists());
}

#[test]
fn a_runtime_writer_creates_the_store_in_its_own_fresh_home() {
    let home = tempfile::tempdir().unwrap();
    let pin = StoreReadPin::capture(home.path());
    let guard = pin.try_exclusive().unwrap();
    assert!(plugins_dir(home.path()).join(".store.lock").is_file());
    guard.for_home(home.path()).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::Busy)
    ));
    drop(guard);
    drop(pin.try_shared().unwrap());
    drop(pin.try_exclusive().unwrap());
}

#[test]
fn a_runtime_writer_never_recreates_a_store_its_reader_pinned() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let pin = StoreReadPin::capture(home.path());
    assert!(pin.is_captured());
    std::fs::remove_dir_all(plugins_dir(home.path())).unwrap();
    assert_eq!(absent(pin.try_exclusive()), StoreAbsent::Missing);
    assert!(!plugins_dir(home.path()).exists());
    bootstrap_store(home.path()).unwrap();
    assert_eq!(absent(pin.try_exclusive()), StoreAbsent::Replaced);
}

#[test]
fn a_pinned_writer_guard_admits_only_its_pinned_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let guard = StoreReadPin::capture(&home).try_exclusive().unwrap();
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    std::fs::create_dir(&home).unwrap();
    let error = guard.for_home(&home).unwrap_err();
    assert!(
        matches!(
            gents_protocol::error_chain::find_source::<PublishedPluginReadError>(error.as_ref()),
            Some(PublishedPluginReadError::Absent(StoreAbsent::Replaced))
        ),
        "{error:#}"
    );
}

#[test]
fn writers_and_readers_still_contend_as_busy() {
    let home = tempfile::tempdir().unwrap();
    let pin = StoreReadPin::capture(home.path());
    let other = StoreReadPin::capture(home.path());
    let held = pin.try_exclusive().unwrap();
    assert!(matches!(
        other.try_exclusive(),
        Err(PublishedPluginReadError::Busy)
    ));
    assert!(PublishedPluginReadError::error_chain_is_busy(
        try_lock_store(home.path()).err().unwrap().as_ref()
    ));
    drop(held);
    let offline = try_lock_store(home.path()).unwrap();
    assert!(matches!(
        pin.try_exclusive(),
        Err(PublishedPluginReadError::Busy)
    ));
    drop(offline);
    let reader = try_lock_store_shared(home.path()).unwrap();
    assert!(matches!(
        pin.try_exclusive(),
        Err(PublishedPluginReadError::Busy)
    ));
    drop(reader);
    drop(pin.try_exclusive().unwrap());
}

#[test]
fn an_offline_writer_initializes_the_home_it_names() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("new");
    drop(try_lock_store(&home).unwrap());
    assert!(plugins_dir(&home).join(".store.lock").is_file());
}
