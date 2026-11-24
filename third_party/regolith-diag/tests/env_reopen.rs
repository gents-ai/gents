//! Reopening a database has to read the filesystem it was written to.
//!
//! Every path `Db::open` takes must go through the configured [`Env`].
//! A single `std::fs` call that slips past it looks fine on the default
//! backend, where `Env` *is* the real filesystem, and makes reopening
//! impossible on every other one: the call looks for the database on
//! disk, does not find it, and the open fails with a bare `NotFound`
//! that names nothing.
//!
//! That is exactly what happened to WAL replay, which opened its log
//! with `std::fs::File::open` while everything around it used `Env`.
//! The default backend never noticed. `MemEnv` is the cheapest backend
//! that would, so it is what guards the rule here.

#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::env::{Env, WriteMode};
use regolith::{Db, MemEnv, Options, WriteBatch};

fn opts(env: &Arc<MemEnv>) -> Options {
    Options::default()
        .env(env.clone())
        .max_background_compactions(0)
}

/// The regression: a database on a non-default `Env` must reopen and
/// still hold everything it acknowledged.
#[test]
fn a_database_on_a_memory_env_reopens_with_every_acknowledged_write() {
    let env = Arc::new(MemEnv::new());

    {
        let db = Db::open("/reopen", opts(&env)).expect("first open");
        for i in 0..200u32 {
            db.put(format!("k{i:04}").as_bytes(), format!("v{i}").as_bytes())
                .expect("put");
        }
        let mut batch = WriteBatch::new();
        batch.put(b"batched", b"yes");
        batch.delete(b"k0000");
        db.write(batch).expect("batch");
        db.close().expect("close");
    }

    let db = Db::open("/reopen", opts(&env)).expect(
        "reopening a database on a non-default Env must work; a NotFound here means some \
         path took std::fs instead of the Env and looked on the real filesystem",
    );
    assert_eq!(db.get(b"k0001").expect("get").as_deref(), Some(&b"v1"[..]));
    assert_eq!(
        db.get(b"batched").expect("get").as_deref(),
        Some(&b"yes"[..])
    );
    assert_eq!(
        db.get(b"k0000").expect("get"),
        None,
        "the batched delete must survive"
    );
    assert_eq!(db.scan(None, None).expect("scan").len(), 200);
    db.close().expect("close");
}

/// Reopening must survive an unclean close, which is the path that
/// actually replays the WAL rather than reading a flushed table.
#[test]
fn a_memory_env_database_replays_its_wal_after_an_unclean_close() {
    let env = Arc::new(MemEnv::new());

    {
        let db = Db::open("/unclean", opts(&env)).expect("first open");
        for i in 0..50u32 {
            db.put(format!("u{i:04}").as_bytes(), b"v").expect("put");
        }
        // No `close`: the data lives only in the WAL, so the reopen has
        // to replay it rather than read a table.
        drop(db);
    }

    let db = Db::open("/unclean", opts(&env)).expect("reopen after an unclean close");
    for i in 0..50u32 {
        assert_eq!(
            db.get(format!("u{i:04}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(&b"v"[..]),
            "u{i:04} did not survive WAL replay through the Env",
        );
    }
    db.close().expect("close");
}

/// A partial tail on the newest log is the ordinary shape of a crash,
/// and recovering from it re-reads the log to decide whether the tail is
/// torn or is damage with whole records behind it.
///
/// That second read is the one the reviewer caught: it used
/// `std::fs::read` while the first went through the `Env`, so a database
/// on any other backend reopened fine until its newest log was partial,
/// and then could not open at all. The default backend cannot show it,
/// because there `Env` *is* the real filesystem.
#[test]
fn a_memory_env_database_reopens_when_its_newest_wal_ends_mid_record() {
    let env = Arc::new(MemEnv::new());

    {
        let db = Db::open("/torn", opts(&env)).expect("first open");
        for i in 0..40u32 {
            db.put(format!("t{i:04}").as_bytes(), b"value")
                .expect("put");
        }
        drop(db);
    }

    // Cut the newest log a few bytes short so its last record is
    // incomplete, which is what a crash mid-append leaves behind.
    let wal = newest_wal(&env, "/torn");
    let full = Env::read(&*env, &wal).expect("read wal");
    assert!(full.len() > 24, "the log needs records to cut into");
    let cut = full.len() - 7;
    write_all(&env, &wal, &full[..cut]);

    let db = Db::open("/torn", opts(&env)).expect(
        "a torn tail on the newest log is a crash artifact, not an unopenable database; a \
         failure here means the torn-tail classifier read the real filesystem instead of \
         the Env",
    );

    // Everything before the cut must still be there. The last record may
    // or may not survive: it is the one that was torn.
    let survived = (0..40u32)
        .filter(|i| {
            db.get(format!("t{i:04}").as_bytes())
                .expect("get")
                .is_some()
        })
        .count();
    assert!(
        survived >= 39,
        "a cut inside the last record must not lose the records before it, {survived}/40 survived",
    );
    db.close().expect("close");
}

/// The newest log in `dir`, by filename order.
fn newest_wal(env: &Arc<MemEnv>, dir: &str) -> std::path::PathBuf {
    let mut logs: Vec<_> = Env::read_dir(&**env, std::path::Path::new(&format!("{dir}/wal")))
        .expect("read_dir wal")
        .map(|e| e.expect("walk wal").path)
        .filter(|p| p.extension().is_some_and(|x| x == "log"))
        .collect();
    logs.sort();
    logs.pop().expect("at least one wal file")
}

fn write_all(env: &Arc<MemEnv>, path: &std::path::Path, bytes: &[u8]) {
    let mut f = Env::open_write(&**env, path, WriteMode::Truncate).expect("open_write");
    f.write_all(bytes).expect("write");
    f.sync_all().expect("sync");
}
