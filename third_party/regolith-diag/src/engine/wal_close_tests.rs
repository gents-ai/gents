//! A clean close ends the log with CLOSE after a sync of everything before
//! it; a close that cannot vouch for the log leaves CLOSE out; and an open
//! reports the tail it drops (`proofs/tla/WalRecovery.tla`, `AppendClose`,
//! `CloseNext` and `Discarded`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::TempDir;

use super::wal_frame::{self, HEADER_LEN, KIND_CLOSE, STAMP_LEN};
use super::wal_rotation_tests::SyncFault;
use crate::statistics::{Statistics, Ticker};
use crate::{Db, EventListener, Options, WalTailDiscardedInfo};

fn log_of(dir: &TempDir) -> PathBuf {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir.path().join("wal"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    logs.sort();
    logs.pop().expect("a log")
}

/// The kind, claim and offset of the last record of the log at `path`.
fn last_record(path: &Path) -> (u8, u64, u64) {
    let bytes = std::fs::read(path).unwrap();
    let nonce = wal_frame::stamp_nonce(bytes[..STAMP_LEN].try_into().unwrap()).unwrap();
    let mut at = STAMP_LEN as u64;
    let mut last = None;
    while (at as usize) < bytes.len() {
        let header = wal_frame::decode_header(&bytes[at as usize..], nonce, at, false).unwrap();
        last = Some((header.kind, header.synced_through, at));
        at += header.record_len();
    }
    last.expect("a record")
}

#[derive(Default)]
struct Reports(std::sync::Mutex<Vec<WalTailDiscardedInfo>>);

impl EventListener for Reports {
    fn on_wal_tail_discarded(&self, info: &WalTailDiscardedInfo) {
        self.0.lock().unwrap().push(info.clone());
    }
}

/// Close flushes what the memtable holds into a table and removes the log
/// it was written to; the next log holds only CLOSE, written before the
/// one sync that makes the stamp and CLOSE durable together, so CLOSE
/// claims nothing synced before it (`WalRecovery.tla`, `CloseNext`).
#[test]
fn close_ends_in_a_next_log_holding_only_close() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.put(b"a", b"1").unwrap();
    db.put(b"b", b"2").unwrap();
    let written = log_of(&dir);
    db.close().unwrap();
    let next = log_of(&dir);
    assert_ne!(next, written, "the log the writes went to is removed");
    assert_eq!(
        std::fs::read_dir(dir.path().join("wal")).unwrap().count(),
        1,
        "one log is left"
    );
    let (kind, claim, at) = last_record(&next);
    assert_eq!(kind, KIND_CLOSE);
    assert_eq!(at, STAMP_LEN as u64, "CLOSE follows the stamp");
    assert_eq!(claim, 0, "no sync came before CLOSE");
    assert_eq!(
        std::fs::metadata(&next).unwrap().len(),
        at + HEADER_LEN as u64
    );
    assert!(
        db.put(b"c", b"3").is_err(),
        "a closed database takes no write"
    );
    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
    assert_eq!(db.get(b"b").unwrap(), Some(b"2".to_vec()));
}

/// A latched database may have bytes at the end of its log nobody can
/// account for, so its close leaves CLOSE out rather than vouch for them.
#[test]
fn a_latched_database_closes_without_appending_close() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.put(b"a", b"1").unwrap();
    db.engine().latch_callback_panic("test");
    db.close().unwrap();
    // Close flushed the memtable into a table and removed its log, so the
    // next log, the one left, holds nothing: not even CLOSE.
    assert_eq!(
        std::fs::metadata(log_of(&dir)).unwrap().len(),
        STAMP_LEN as u64
    );
    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
}

/// The first reason latched is the one every later write reports: it is the
/// one that explains the rest. Latches racing from many threads keep one
/// reason, the same for every reader, and the latch never opens again.
#[test]
fn the_first_latched_reason_is_the_one_kept() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert!(!db.engine().is_latched());
    db.engine()
        .latch_wal_failure(&std::io::Error::other("the first failure"));
    db.engine().latch_callback_panic("a later panic");
    db.engine()
        .latch_wal_failure(&std::io::Error::other("a later failure"));
    assert!(db.engine().is_latched());
    let refused = db.put(b"k", b"v").unwrap_err().to_string();
    assert!(
        refused.contains("the first failure"),
        "a later reason replaced the first: {refused}"
    );

    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    std::thread::scope(|scope| {
        for t in 0..8 {
            let db = &db;
            scope.spawn(move || {
                db.engine()
                    .latch_wal_failure(&std::io::Error::other(format!("failure {t}")));
            });
        }
    });
    let kept = db.put(b"k", b"v").unwrap_err().to_string();
    assert!(kept.contains("failure "), "no reason was kept: {kept}");
    for _ in 0..8 {
        assert_eq!(db.put(b"k", b"v").unwrap_err().to_string(), kept);
    }
}

/// A close whose sync fails says why and latches the database, since what
/// reached the device is unknown. The log held no record, so its one sync
/// came after CLOSE: the log holds the stamp and at most CLOSE, which
/// vouches for nothing but the stamp.
#[test]
fn a_close_whose_sync_fails_latches() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), Options::default()).unwrap();
    db.put(b"a", b"1").unwrap();
    // Flushed now, so close has no memtable to seal and the sync that
    // fails is the log's own.
    db.flush().unwrap();
    {
        let _fault = SyncFault::arm(&dir);
        let err = db.close().unwrap_err();
        assert!(
            err.to_string().contains("injected WAL sync failure"),
            "{err}"
        );
    }
    let left = std::fs::read(log_of(&dir)).unwrap();
    assert!(
        left.len() == STAMP_LEN || left.len() == STAMP_LEN + HEADER_LEN,
        "the log holds no record: {} bytes",
        left.len()
    );
    let later = db.put(b"b", b"2").unwrap_err();
    assert!(later.to_string().contains("unknown state"), "{later}");
    drop(db);
    let db = Db::open(dir.path(), Options::default()).unwrap();
    assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
}

/// A read-only open drops the damaged tail from what it serves and
/// reports it, but writes nothing: the bytes stay for the next read-write
/// open, which reports them again and truncates them.
#[test]
fn a_read_only_open_reports_the_tail_and_leaves_it_in_place() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), Options::default()).unwrap();
        db.put(b"a", b"1").unwrap();
    }
    let log = log_of(&dir);
    let mut bytes = std::fs::read(&log).unwrap();
    let whole = bytes.len() as u64;
    bytes.extend_from_slice(&[0xAB; 100]);
    std::fs::write(&log, &bytes).unwrap();

    let open_with_reports = |read_only: bool| {
        let reports = Arc::new(Reports::default());
        let stats = Arc::new(Statistics::new());
        let opts = Options::default()
            .listeners(vec![reports.clone() as Arc<dyn EventListener>])
            .statistics(Some(Arc::clone(&stats)));
        let db = if read_only {
            Db::open_read_only(dir.path(), opts)
        } else {
            Db::open(dir.path(), opts)
        }
        .unwrap();
        assert_eq!(db.get(b"a").unwrap(), Some(b"1".to_vec()));
        let taken = reports.0.lock().unwrap().clone();
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].file_path, log);
        assert_eq!(taken[0].offset, whole);
        assert_eq!(taken[0].discarded_bytes, 100);
        assert_eq!(taken[0].last_sequence, 1);
        assert_eq!(stats.get_ticker(Ticker::WalTailDiscarded), 1);
        assert_eq!(stats.get_ticker(Ticker::WalTailDiscardedBytes), 100);
    };
    open_with_reports(true);
    assert_eq!(
        std::fs::read(&log).unwrap(),
        bytes,
        "a read-only open writes nothing"
    );
    open_with_reports(false);
}
