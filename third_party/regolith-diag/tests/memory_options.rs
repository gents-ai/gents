//! `Options::memory()`: a database that never touches a filesystem.
//!
//! The preset exists for one non-obvious constraint. `MemEnv` starts no
//! threads, so a background compaction worker cannot exist on it and asking
//! for one fails the open. A caller assembling the options by hand meets that
//! as an open error that says nothing about the environment.

use regolith::{
    Db, DurabilityMode, IsolationLevel, MemEnv, OptimisticTransactionDb, Options, TxnOptions,
};
use std::sync::Arc;

#[test]
fn a_memory_database_reads_back_what_it_writes() {
    let db = Db::open("memory-basic", Options::memory()).unwrap();
    db.put(b"k", b"v").unwrap();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(b"v".as_slice()));
    db.delete(b"k").unwrap();
    assert_eq!(db.get(b"k").unwrap(), None);
}

#[test]
fn a_memory_database_scans_and_transacts() {
    let db = OptimisticTransactionDb::open("memory-txn", Options::memory()).unwrap();
    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
    for index in 0..64u32 {
        txn.put(format!("key:{index:04}").as_bytes(), b"v").unwrap();
    }
    txn.commit().unwrap();

    let scanned: Vec<Vec<u8>> = db
        .db()
        .scan_stream(Some(b"key:"), Some(b"key;"))
        .unwrap()
        .map(|entry| entry.unwrap().0)
        .collect();
    assert_eq!(scanned.len(), 64);
}

/// Two of them are separate databases, not one shared store.
#[test]
fn two_memory_databases_do_not_share_state() {
    let first = Db::open("memory-isolation", Options::memory()).unwrap();
    let second = Db::open("memory-isolation", Options::memory()).unwrap();

    first.put(b"only-in-first", b"v").unwrap();
    assert_eq!(second.get(b"only-in-first").unwrap(), None);
}

/// The path is a name, not a location: nothing is created on disk.
#[test]
fn a_memory_database_creates_nothing_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("never-created");

    let db = Db::open(&path, Options::memory()).unwrap();
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    drop(db);

    assert!(!path.exists(), "a memory database wrote to the filesystem");
}

/// Enough writes to force a flush and a compaction, which have to run on the
/// calling thread because the environment cannot spawn one.
#[test]
fn a_memory_database_compacts_without_a_worker_thread() {
    let db = Db::open("memory-compaction", Options::memory()).unwrap();
    for index in 0..8_000u32 {
        db.put(format!("key:{index:06}").as_bytes(), &[b'v'; 128])
            .unwrap();
    }
    db.flush().unwrap();

    assert_eq!(
        db.get(b"key:000000").unwrap().as_deref(),
        Some([b'v'; 128].as_slice())
    );
    assert_eq!(
        db.get(b"key:007999").unwrap().as_deref(),
        Some([b'v'; 128].as_slice())
    );
}

#[test]
fn the_preset_is_eventual_and_worker_free() {
    let options = Options::memory();
    assert_eq!(options.get_max_background_compactions(), 0);
    assert_eq!(options.get_durability(), DurabilityMode::Eventual);
    assert!(!options.get_env().capabilities().threads);
    options.validate().unwrap();
}

/// The trap the preset exists to remove, reported against the option that
/// caused it and naming the environment rather than failing inside `open`.
#[test]
fn asking_a_thread_free_environment_for_a_worker_is_refused() {
    let options = Options::memory().max_background_compactions(1);

    let error = options.validate().expect_err("must not validate");
    let message = error.to_string();
    assert!(
        message.contains("max_background_compactions"),
        "names the option: {message}"
    );
    assert!(
        message.contains("MemEnv"),
        "names the environment: {message}"
    );

    Db::open("memory-invalid", options).expect_err("open must refuse it too");
}

/// The same rejection for a hand-assembled MemEnv, which is how a caller
/// arrives here without the preset.
#[test]
fn a_hand_built_mem_env_with_a_worker_is_refused() {
    let options = Options::default().env(Arc::new(MemEnv::new()));
    assert!(
        options.get_max_background_compactions() > 0,
        "the default asks for a worker, which is what makes this a trap"
    );
    options.validate().expect_err("must not validate");
}

/// A filesystem-backed database still gets its worker: the check keys off the
/// environment, not off a blanket ban.
#[test]
fn a_filesystem_database_still_gets_its_worker() {
    let options = Options::default();
    assert!(options.get_env().capabilities().threads);
    assert!(options.get_max_background_compactions() > 0);
    options.validate().unwrap();
}
