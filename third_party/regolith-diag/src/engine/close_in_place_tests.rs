//! Close seals the last memtable in place (E46): the log its writes went to
//! is never synced, it stays the newest until the flush that makes it
//! redundant is durable, and the next log is created only after that
//! (`proofs/tla/WalRecovery.tla`, `SealInPlace`, `FlushInPlace`,
//! `CloseNext`, RED `NewLogBeforeFlush`). A close that fails part way syncs
//! that log before a newer one exists, and takes writes again.

use std::sync::Arc;

use tempfile::TempDir;

use super::recovery_tests::RecordingEnv;
use super::wal_frame::{HEADER_LEN, STAMP_LEN};
use crate::{Db, Options};

/// A database through the recording Env, with no worker, so every flush
/// runs on the thread that closes.
fn recorded(dir: &TempDir) -> (Db, Arc<RecordingEnv>) {
    let recording = Arc::new(RecordingEnv::default());
    let options = Options::default()
        .env(recording.clone())
        .max_background_compactions(0);
    (Db::open(dir.path(), options).unwrap(), recording)
}

/// What the host saw from here on: the recording is cleared.
fn from_now(recording: &RecordingEnv) {
    *recording.events.lock().unwrap() = Default::default();
}

fn ops(recording: &RecordingEnv) -> Vec<String> {
    recording.events.lock().unwrap().ops.clone()
}

fn position(ops: &[String], op: &str) -> usize {
    ops.iter()
        .position(|o| o == op)
        .unwrap_or_else(|| panic!("no {op:?} in {ops:?}"))
}

/// The log the writes went to is removed unsynced, and only then is the
/// next log created; its stamp and CLOSE are synced together, the close's
/// one log sync.
#[test]
fn close_never_syncs_the_log_it_sealed_in_place() {
    let dir = TempDir::new().unwrap();
    let (db, recording) = recorded(&dir);
    for i in 0..100u32 {
        db.put(&i.to_be_bytes(), &[7; 512]).unwrap();
    }
    from_now(&recording);
    db.close().unwrap();
    let ops = ops(&recording);
    let syncs = ops.iter().filter(|o| *o == "sync").count();
    assert_eq!(syncs, 1, "one log sync, the next log's: {ops:?}");
    let removed = position(&ops, "remove wal_000001.log");
    let created = ops
        .iter()
        .position(|o| o.starts_with("create wal_"))
        .unwrap_or_else(|| panic!("no next log in {ops:?}"));
    assert!(removed < created, "the next log after the removal: {ops:?}");
    assert_eq!(
        ops[created + 1..created + 4],
        [
            format!("write {STAMP_LEN}"),
            format!("write {HEADER_LEN}"),
            "sync".to_string()
        ],
        "stamp, CLOSE, one sync: {ops:?}"
    );
    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    for i in 0..100u32 {
        assert_eq!(db.get(&i.to_be_bytes()).unwrap(), Some(vec![7; 512]));
    }
}

/// With nothing in memory there is nothing to seal: close closes the log it
/// has, which holds no record, with one sync after CLOSE.
#[test]
fn a_close_with_nothing_in_memory_closes_the_log_it_has() {
    let dir = TempDir::new().unwrap();
    let (db, recording) = recorded(&dir);
    db.put(b"k", b"v").unwrap();
    db.flush().unwrap();
    from_now(&recording);
    db.close().unwrap();
    let ops = ops(&recording);
    assert!(
        !ops.iter()
            .any(|o| o.starts_with("create ") || o.starts_with("remove ")),
        "no log created or removed: {ops:?}"
    );
    assert_eq!(
        ops[..2],
        [format!("write {HEADER_LEN}"), "sync".to_string()],
        "CLOSE, then one sync: {ops:?}"
    );
}

/// A close whose flush fails after it sealed the last memtable in place
/// syncs that memtable's log before the next log exists (E2), then takes
/// writes in the next log; nothing is lost, and the next close, with the
/// cause gone, finishes.
#[test]
fn a_close_that_fails_after_sealing_in_place_syncs_before_the_next_log() {
    let dir = TempDir::new().unwrap();
    let (db, recording) = recorded(&dir);
    for i in 0..50u32 {
        db.put(&i.to_be_bytes(), &[3; 256]).unwrap();
    }
    // The table the close's flush writes: the open's log is 1, the table 2.
    let blocked = dir.path().join("sst").join("000002.sst");
    std::fs::create_dir(&blocked).unwrap();
    from_now(&recording);
    db.close().unwrap_err();
    let ops = ops(&recording);
    assert!(
        !ops.iter().any(|o| o.starts_with("remove ")),
        "the sealed log stays: {ops:?}"
    );
    let synced = position(&ops, "sync");
    let created = ops
        .iter()
        .position(|o| o.starts_with("create wal_"))
        .unwrap_or_else(|| panic!("no next log in {ops:?}"));
    assert!(
        synced < created,
        "the sealed log is synced before the next log exists: {ops:?}"
    );

    for i in 50..80u32 {
        db.put(&i.to_be_bytes(), &[4; 256]).unwrap();
    }
    std::fs::remove_dir(&blocked).unwrap();
    db.close().unwrap();
    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    for i in 0..80u32 {
        let want = if i < 50 { vec![3; 256] } else { vec![4; 256] };
        assert_eq!(db.get(&i.to_be_bytes()).unwrap(), Some(want), "key {i}");
    }
}
