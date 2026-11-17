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
    let bootstrap = bootstrap_store(home.path()).unwrap();
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
    drop(bootstrap_store(&home).unwrap());
    let pin = StoreReadIdentity::capture(&home).unwrap();
    std::fs::rename(&home, root.path().join("old-home")).unwrap();
    std::fs::create_dir(&home).unwrap();
    assert!(matches!(
        pin.try_shared(),
        Err(PublishedPluginReadError::StoreAbsent(StoreAbsent::Replaced))
    ));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 0);
    // Even an independently bootstrapped new home cannot be adopted by pin.
    drop(bootstrap_store(&home).unwrap());
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
fn a_missing_pin_never_adopts_a_later_bootstrap() {
    let home = tempfile::tempdir().unwrap();
    let pin = StoreReadPin::capture(home.path());
    drop(bootstrap_store(home.path()).unwrap());
    let error = pin.try_shared().err().unwrap();
    assert!(PublishedPluginReadError::error_chain_is_absent(&error));
}

#[test]
fn acquisition_decisions_replay_the_lean_owner() {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut inputs = Vec::new();
    let mut actual = Vec::new();
    for mode in [Mode::Read, Mode::Bootstrap, Mode::Writer] {
        for bits in 0..16 {
            let observation = Observation {
                same_home: bits & 1 != 0,
                store_present: bits & 2 != 0,
                lock_present: bits & 4 != 0,
                contended: bits & 8 != 0,
            };
            inputs.push(serde_json::json!({"mode":mode,"observation":observation}));
            actual.push(acquire(mode, observation));
        }
    }
    let dir = gents_lean_contract::proofs_dir().unwrap();
    gents_lean_contract::run_lake_build(&dir).unwrap();
    let mut lean = Command::new("lake")
        .args([
            "env",
            "lean",
            "--run",
            "Proofs/Conformance/PluginStoreHome.lean",
        ])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(lean.stdin.take().unwrap(), "{}", serde_json::json!(inputs)).unwrap();
    let output = lean.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected: Vec<Decision> = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(actual, expected);
}
