use super::*;

#[test]
fn absent_read_creates_neither_directory_nor_lock() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("missing");
    assert!(matches!(
        try_lock_store_shared(&home),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    assert!(!home.exists());
    std::fs::create_dir(&home).unwrap();
    assert!(matches!(
        try_lock_store_shared(&home),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    std::fs::create_dir(home.join("plugins")).unwrap();
    assert!(matches!(
        try_lock_store_shared(&home),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    assert_eq!(std::fs::read_dir(home.join("plugins")).unwrap().count(), 0);
}

#[test]
fn bootstrap_then_read_and_both_writer_contentions() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let bootstrap = try_lock_store(home.path()).unwrap();
    assert!(matches!(
        try_lock_store_shared(home.path()),
        Err(PublishedPluginReadError::Busy)
    ));
    assert!(try_lock_store(home.path()).is_err());
    drop(bootstrap);
    let reader = try_lock_store_shared(home.path()).unwrap();
    assert!(try_lock_store_shared(home.path()).is_ok());
    assert!(try_lock_store(home.path()).is_err());
    drop(reader);
    assert!(try_lock_store(home.path()).is_ok());
}

#[test]
fn pinned_read_refuses_a_replaced_home_or_lock_without_writing() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    let pin = StoreReadIdentity::capture(&home).unwrap();
    std::fs::rename(&home, root.path().join("old-home")).unwrap();
    std::fs::create_dir(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    // Even an independently bootstrapped new home cannot be adopted by pin.
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    let pin = StoreReadIdentity::capture(&home).unwrap();
    std::fs::rename(lock_path(&home), home.join("old-lock")).unwrap();
    std::fs::File::create(lock_path(&home)).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
}

#[test]
fn a_missing_store_pin_recaptures_after_bootstrap_in_the_original_home_only() {
    let home = tempfile::tempdir().unwrap();
    let pin = StoreReadPin::capture(home.path());
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    bootstrap_store(home.path()).unwrap();
    assert!(pin.try_shared().is_ok());
}

#[test]
fn an_uncaptured_store_cannot_adopt_a_replaced_existing_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let pin = StoreReadPin::capture(&home);
    std::fs::rename(&home, root.path().join("old")).unwrap();
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
}

#[test]
fn capture_pins_an_existing_store_before_the_first_read() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let pin = StoreReadPin::capture(home.path());
    assert!(pin.is_captured());
    std::fs::rename(home.path().join("plugins"), home.path().join("retired")).unwrap();
    bootstrap_store(home.path()).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
}

#[test]
fn an_initially_absent_home_can_capture_its_first_store_but_not_a_later_one() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let pin = StoreReadPin::capture(&home);
    assert!(!pin.is_captured());
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    assert!(!home.exists());
    bootstrap_store(&home).unwrap();
    assert!(pin.try_shared().is_ok());
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
}

#[test]
fn bootstrap_does_not_contend_with_existing_shared_readers() {
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let first = try_lock_store_shared(home.path()).unwrap();
    bootstrap_store(home.path()).unwrap();
    let second = try_lock_store_shared(home.path()).unwrap();
    assert!(try_lock_store(home.path()).is_err());
    drop((first, second));
    assert!(try_lock_store(home.path()).is_ok());
}

#[test]
fn a_captured_removed_home_is_missing_and_never_recreated() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    bootstrap_store(&home).unwrap();
    let pin = StoreReadPin::capture(&home);
    std::fs::remove_dir_all(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    assert!(!home.exists());
}

#[test]
fn acquisition_decisions_replay_the_lean_owner() {
    let mut inputs = Vec::new();
    let mut actual = Vec::new();
    for bits in 0..16 {
        let observation = Observation {
            same_home: bits & 1 != 0,
            store_present: bits & 2 != 0,
            lock_present: bits & 4 != 0,
            contended: bits & 8 != 0,
        };
        inputs.push(serde_json::json!({"observation":observation}));
        actual.push(acquire(observation));
    }
    let dir = gents_lean_contract::proofs_dir().unwrap();
    let output = gents_lean_contract::run_prepared_generator_with_input(
        &dir,
        "Proofs.Conformance.PluginStoreHome:olean",
        "Proofs/Conformance/PluginStoreHome.lean",
        &serde_json::json!(inputs).to_string(),
    )
    .unwrap();
    let expected: Vec<Decision> = serde_json::from_str(&output).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn pinned_reader_refuses_a_replaced_store_and_does_not_recreate_a_removed_home() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    bootstrap_store(&home).unwrap();
    let pin = StoreReadIdentity::capture(&home).unwrap();
    std::fs::rename(home.join("plugins"), home.join("retired-plugins")).unwrap();
    std::fs::create_dir(home.join("plugins")).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    assert_eq!(std::fs::read_dir(home.join("plugins")).unwrap().count(), 0);
    let pin = StoreReadPin::capture(&home);
    std::fs::remove_dir_all(&home).unwrap();
    let error = pin.try_shared().err().unwrap();
    assert!(matches!(
        error,
        PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing)
    ));
    assert!(!home.exists());
}

#[cfg(unix)]
#[test]
fn existing_shared_lock_needs_no_write_permission() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    bootstrap_store(home.path()).unwrap();
    let lock = lock_path(home.path());
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o400)).unwrap();
    let pin = StoreReadIdentity::capture(home.path()).unwrap();
    assert!(pin.try_shared().is_ok());
    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn concurrent_first_store_captures_share_one_immutable_identity() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let pin = StoreReadPin::capture(&home);
    bootstrap_store(&home).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let captures = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..16)
            .map(|_| {
                let pin = pin.clone();
                let barrier = barrier.clone();
                scope.spawn(move || {
                    barrier.wait();
                    pin.identity().unwrap()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(captures
        .iter()
        .all(|identity| Arc::ptr_eq(identity, &captures[0])));
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    assert!(Arc::ptr_eq(&pin.identity().unwrap(), &captures[0]));
}

#[test]
fn initially_absent_home_is_retained_even_when_first_store_capture_fails() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let pin = StoreReadPin::capture(&home);
    std::fs::create_dir(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Missing))
    ));
    std::fs::rename(&home, root.path().join("retired")).unwrap();
    bootstrap_store(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    assert!(!pin.is_captured());
}
