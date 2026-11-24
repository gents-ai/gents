//! A rotation seals the active log, and recovery tolerates a torn tail only in
//! the newest log, so the rotation has to make the log it seals durable before
//! a newer one exists. These tests fail the sync and watch what the engine
//! does; the power-cut proof is `tests/sealed_wal_power_loss.rs`.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::engine::wal::fault;
use crate::{Db, Options};

/// Fails every WAL sync under one directory until dropped.
pub(super) struct SyncFault(PathBuf);

impl SyncFault {
    pub(super) fn arm(dir: &TempDir) -> Self {
        fault::arm_sync_failure(dir.path());
        Self(dir.path().to_path_buf())
    }
}

impl Drop for SyncFault {
    fn drop(&mut self) {
        fault::disarm_sync_failure(&self.0);
    }
}

/// Default `Eventual` durability, so no commit syncs the log and the only sync
/// a test can see is the rotation's.
fn small_db(dir: &TempDir) -> Db {
    let options = Options::default()
        .write_buffer_size(4096)
        .max_background_compactions(0);
    Db::open(dir.path(), options).unwrap()
}

fn wal_files(dir: &TempDir) -> usize {
    std::fs::read_dir(dir.path().join("wal")).unwrap().count()
}

#[test]
fn a_write_that_fills_the_memtable_fails_when_the_sealed_log_cannot_be_synced() {
    let dir = TempDir::new().unwrap();
    let db = small_db(&dir);
    let _fault = SyncFault::arm(&dir);

    let value = [b'v'; 256];
    let err = (0..1000u32)
        .find_map(|i| crate::through_stalls(|| db.put(&i.to_be_bytes(), &value)).err())
        .expect("filling the memtable must rotate it and surface the sync failure");
    assert!(
        err.to_string().contains("injected WAL sync failure"),
        "{err}"
    );
    assert_eq!(
        wal_files(&dir),
        1,
        "no newer log may exist beside an unsynced one"
    );

    let later = db.put(b"later", b"v").unwrap_err();
    assert!(later.to_string().contains("unknown state"), "{later}");
}

#[test]
fn a_flush_fails_when_the_log_it_seals_cannot_be_synced() {
    let dir = TempDir::new().unwrap();
    let db = small_db(&dir);
    crate::through_stalls(|| db.put(b"k", b"v")).unwrap();
    let _fault = SyncFault::arm(&dir);

    let err = db.flush().unwrap_err();
    assert!(
        err.to_string().contains("injected WAL sync failure"),
        "{err}"
    );
    assert_eq!(
        wal_files(&dir),
        1,
        "no newer log may exist beside an unsynced one"
    );
}
