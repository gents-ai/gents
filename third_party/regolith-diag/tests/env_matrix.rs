//! The database lifecycle, run against more than one [`Env`].
//!
//! Every other test in this suite runs on [`StdEnv`], so a leftover
//! `std::fs` call inside the engine would still pass all of them.
//! [`MemEnv`] has no filesystem behind it at all: the same lifecycle
//! run against it fails loudly the moment the engine reaches around
//! the abstraction instead of through it.
//!
//! `MemEnv` also cannot start a thread, cannot hard-link, and cannot
//! fsync a directory, so these tests double as the check that regolith
//! degrades around a missing capability instead of claiming one.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads, the filesystem or proptest, none of which exist there. The browser
// suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, Env, MemEnv, Options, WriteBatch};
use std::path::Path;
use std::sync::Arc;

/// Options that a single-threaded, in-memory environment can serve:
/// no background worker, and small enough to cross a flush boundary
/// inside a test.
fn mem_options(env: &MemEnv) -> Options {
    Options::default()
        .env(Arc::new(env.clone()))
        .max_background_compactions(0)
        .write_buffer_size(4 * 1024)
        .block_cache_size(64 * 1024)
        .target_file_size(16 * 1024)
        .level_base_bytes(64 * 1024)
        .l0_compaction_trigger(2)
}

fn open_mem(env: &MemEnv, path: &str) -> Db {
    Db::open(Path::new(path), mem_options(env)).expect("open on MemEnv")
}

#[test]
fn full_lifecycle_runs_entirely_in_memory() {
    let env = MemEnv::new();
    let db = open_mem(&env, "/db");

    // put / get
    regolith::through_stalls(|| db.put(b"alpha", b"one")).unwrap();
    regolith::through_stalls(|| db.put(b"bravo", b"two")).unwrap();
    regolith::through_stalls(|| db.put(b"charlie", b"three")).unwrap();
    assert_eq!(db.get(b"bravo").unwrap().as_deref(), Some(&b"two"[..]));

    // delete
    regolith::through_stalls(|| db.delete(b"bravo")).unwrap();
    assert_eq!(db.get(b"bravo").unwrap(), None);

    // batch
    let mut batch = WriteBatch::new();
    batch.put(b"delta", b"four");
    batch.delete(b"alpha");
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    assert_eq!(db.get(b"alpha").unwrap(), None);
    assert_eq!(db.get(b"delta").unwrap().as_deref(), Some(&b"four"[..]));

    // snapshot isolation
    let snap = db.snapshot();
    regolith::through_stalls(|| db.put(b"charlie", b"rewritten")).unwrap();
    assert_eq!(
        snap.get(b"charlie").unwrap().as_deref(),
        Some(&b"three"[..])
    );
    assert_eq!(
        db.get(b"charlie").unwrap().as_deref(),
        Some(&b"rewritten"[..])
    );
    drop(snap);

    // scan
    let scanned = db.scan(None, None).unwrap();
    let keys: Vec<Vec<u8>> = scanned.iter().map(|(k, _)| k.clone()).collect();
    assert_eq!(keys, vec![b"charlie".to_vec(), b"delta".to_vec()]);

    // iterate
    let mut iter = db.iter();
    iter.seek_to_first();
    let mut seen = Vec::new();
    while iter.valid() {
        seen.push(iter.key().unwrap().to_vec());
        iter.next();
    }
    iter.status().unwrap();
    assert_eq!(seen, keys);

    // enough volume to cross a flush boundary and produce real
    // SSTables, all of them living in the MemEnv map
    for i in 0..2_000u32 {
        regolith::through_stalls(|| db.put(format!("k{i:06}").as_bytes(), &[b'v'; 64])).unwrap();
    }
    assert_eq!(
        db.get(b"k001999").unwrap().as_deref(),
        Some(&[b'v'; 64][..])
    );

    // compact
    db.compact_range(None, None).wait().unwrap();
    assert_eq!(
        db.get(b"k000042").unwrap().as_deref(),
        Some(&[b'v'; 64][..])
    );

    // close, reopen, read back
    db.close().unwrap();
    drop(db);

    assert!(env.file_count() > 0, "the database must live in the MemEnv");

    let reopened = open_mem(&env, "/db");
    assert_eq!(
        reopened.get(b"k000042").unwrap().as_deref(),
        Some(&[b'v'; 64][..])
    );
    assert_eq!(
        reopened.get(b"charlie").unwrap().as_deref(),
        Some(&b"rewritten"[..])
    );
    assert_eq!(reopened.get(b"alpha").unwrap(), None, "delete persisted");
    reopened.close().unwrap();
}

#[test]
fn nothing_from_a_mem_env_database_reaches_the_real_filesystem() {
    let env = MemEnv::new();
    let db = open_mem(&env, "/no-such-place/db");
    for i in 0..500u32 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"v")).unwrap();
    }
    db.close().unwrap();

    assert!(!Path::new("/no-such-place").exists());
    assert!(env.file_count() > 0);
    assert!(env.total_bytes() > 0);
}

#[test]
fn capabilities_report_what_the_environment_actually_provides() {
    let env = MemEnv::new();
    let db = open_mem(&env, "/db");
    let caps = db.capabilities();
    assert!(!caps.hard_link);
    assert!(!caps.sync_dir);
    assert!(!caps.threads);
    assert!(!caps.file_lock);
    assert!(!caps.durable_sync);
    assert!(caps.atomic_rename);
    db.close().unwrap();

    let std_caps = {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open(dir.path(), Options::default()).unwrap();
        let caps = db.capabilities();
        db.close().unwrap();
        caps
    };
    assert!(std_caps.hard_link);
    assert!(std_caps.atomic_rename);
    assert!(std_caps.durable_sync);
}

#[test]
fn a_checkpoint_copies_bytes_when_the_environment_has_no_hard_links() {
    let env = MemEnv::new();
    let db = open_mem(&env, "/db");
    for i in 0..500u32 {
        regolith::through_stalls(|| db.put(format!("k{i:04}").as_bytes(), b"value")).unwrap();
    }
    db.flush().unwrap();

    regolith::Checkpoint::new(&db)
        .unwrap()
        .create("/checkpoint")
        .wait()
        .expect("checkpoint must fall back to copying without hard links");
    db.close().unwrap();
    drop(db);

    let restored = open_mem(&env, "/checkpoint");
    assert_eq!(
        restored.get(b"k0250").unwrap().as_deref(),
        Some(&b"value"[..])
    );
    restored.close().unwrap();
}

#[test]
fn a_ttl_database_is_refused_when_the_environment_has_no_wall_clock() {
    let env = MemEnv::new();
    env.set_clocks(Some(0), None);
    let err = regolith::DbWithTtl::open(Path::new("/ttl"), mem_options(&env), 60)
        .expect_err("a TTL database needs a wall clock");
    assert!(
        err.to_string().contains("wall clock"),
        "the error must name the missing clock, got: {err}"
    );
}

#[test]
fn a_backup_is_refused_when_the_environment_has_no_wall_clock() {
    let env = MemEnv::new();
    env.set_clocks(Some(0), None);
    let db = open_mem(&env, "/db");
    regolith::through_stalls(|| db.put(b"k", b"v")).unwrap();
    db.flush().unwrap();

    let mut backups =
        regolith::BackupEngine::open_with_env("/backups", Arc::new(env.clone())).unwrap();
    let err = backups
        .create_backup(&db)
        .expect_err("a backup records when it was taken");
    assert!(
        err.to_string().contains("wall clock"),
        "the error must name the missing clock, got: {err}"
    );
    db.close().unwrap();
}

#[test]
fn handles_reopen_cleanly_and_leave_no_lock_file_behind() {
    // `Capabilities::file_lock` is false here: no *process* is
    // excluded. A second handle inside this process still is, and
    // releasing the first must free the path again - a lock that
    // outlived its handle is what made the deleted LOCK-file proxy
    // turn any unclean shutdown into an unopenable database.
    let env = MemEnv::new();
    let first = open_mem(&env, "/db");
    first.put(b"k", b"v").unwrap();
    assert!(
        Db::open("/db", mem_options(&env)).is_err(),
        "a second read-write handle on one directory must be refused"
    );
    first.close().unwrap();
    drop(first);

    let second = open_mem(&env, "/db");
    assert_eq!(second.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
    second.close().unwrap();

    assert!(
        !env.exists(Path::new("/db/LOCK")),
        "no LOCK file is created where there is no cross-process lock"
    );
}
