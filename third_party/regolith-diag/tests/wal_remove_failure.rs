//! A write-ahead log a flush could not remove (E30).
//!
//! A flush puts its memtable in a table and then removes the log that held
//! those writes. When the removal fails the log stays on disk, and recovery
//! must not replay it: its versions would land in the memtable, which a
//! read consults before any table, so a key rewritten since, by a later
//! flush or an ingest, would read back as its older value. The failure is
//! reported, with a warn line and `regolith.wal.remove_failed`, and the log
//! is removed again by the next flush or the next open.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::faulty_env::{FaultyEnv, Refuse};
use regolith::env::Env;
use regolith::{Db, IngestOptions, Options, SstFileWriter, Statistics, Ticker};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    env: Arc<FaultyEnv>,
    stats: Arc<Statistics>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            env: Arc::new(FaultyEnv::default()),
            stats: Arc::new(Statistics::new()),
        }
    }

    fn open(&self) -> Db {
        Db::open(
            self.dir.path(),
            Options::default()
                .env(Arc::clone(&self.env) as Arc<dyn Env>)
                .statistics(Some(Arc::clone(&self.stats)))
                .max_background_compactions(0),
        )
        .unwrap()
    }

    fn logs(&self) -> Vec<PathBuf> {
        common::faulty_env::logs(self.dir.path())
    }

    fn remove_failures(&self) -> u64 {
        self.stats.get_ticker(Ticker::WalRemoveFailed)
    }

    /// Write `key = old` and flush it while the removal of the log that
    /// held the write fails, so the log outlives its flush. Returns the
    /// leftover log; its removal stays refused until the caller disarms.
    fn flush_leaving_its_log(&self, db: &Db, key: &[u8]) -> PathBuf {
        db.put(key, b"old").unwrap();
        let sealed = self.logs();
        assert_eq!(sealed.len(), 1, "one active log before the flush");
        self.env.arm(Refuse::Log(sealed[0].clone()));
        db.flush().unwrap();
        assert!(
            self.env.refused.load(Ordering::SeqCst) > 0,
            "the flush tried to remove its log"
        );
        assert!(sealed[0].exists(), "the flushed log outlived its flush");
        sealed[0].clone()
    }
}

#[test]
fn a_log_a_flush_left_behind_is_not_replayed_above_a_newer_table() {
    let f = Fixture::new();
    let leftover = {
        let db = f.open();
        let leftover = f.flush_leaving_its_log(&db, b"k");
        db.put(b"k", b"new").unwrap();
        // The newer table's flush retries the leftover's removal, which
        // still fails, so the leftover is there at the reopen.
        db.flush().unwrap();
        db.close().unwrap();
        leftover
    };
    f.env.arm(Refuse::Nothing);
    assert!(leftover.exists(), "the leftover survives to the reopen");
    let db = f.open();
    assert_eq!(
        db.get(b"k").unwrap().as_deref(),
        Some(&b"new"[..]),
        "recovery replayed a flushed log above the newer table"
    );
}

#[test]
fn a_log_a_flush_left_behind_is_not_replayed_above_an_ingested_table() {
    let f = Fixture::new();
    {
        let db = f.open();
        f.flush_leaving_its_log(&db, b"k");
        let source = f.dir.path().with_extension("ingest.sst");
        let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
        writer.put(b"k", b"new").unwrap();
        writer.finish().unwrap();
        db.ingest_external_files(&[source], IngestOptions::default())
            .wait()
            .unwrap();
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
        db.close().unwrap();
    }
    f.env.arm(Refuse::Nothing);
    let db = f.open();
    assert_eq!(
        db.get(b"k").unwrap().as_deref(),
        Some(&b"new"[..]),
        "recovery replayed a flushed log above the ingested table"
    );
}

#[test]
fn a_failed_removal_is_counted_and_retried_by_the_next_flush() {
    let f = Fixture::new();
    let db = f.open();
    let leftover = f.flush_leaving_its_log(&db, b"k");
    f.env.arm(Refuse::Nothing);
    assert_eq!(f.remove_failures(), 1, "the failed removal is counted once");

    db.put(b"j", b"v").unwrap();
    db.flush().unwrap();
    assert!(
        !leftover.exists(),
        "the next flush removes the log the earlier one left"
    );
    assert_eq!(f.logs().len(), 1, "only the active log remains");
    assert_eq!(f.remove_failures(), 1, "the retry succeeded");
}

#[test]
fn a_failed_removal_is_retried_by_the_next_open() {
    let f = Fixture::new();
    let leftover = {
        let db = f.open();
        let leftover = f.flush_leaving_its_log(&db, b"k");
        db.close().unwrap();
        leftover
    };
    f.env.arm(Refuse::Nothing);
    assert!(leftover.exists());

    let db = f.open();
    assert!(
        !leftover.exists(),
        "the open removes the log the flush left"
    );
    assert_eq!(f.logs().len(), 1, "only the active log remains");
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"old"[..]));
}

#[test]
fn an_open_that_cannot_remove_a_log_reports_it_and_still_opens() {
    let f = Fixture::new();
    {
        let db = f.open();
        db.put(b"k", b"v").unwrap();
        db.close().unwrap();
    }
    f.env.arm(Refuse::EveryLog);
    let db = f.open();
    f.env.arm(Refuse::Nothing);
    assert!(f.remove_failures() >= 1, "the failed removal is counted");
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
    drop(db);

    let db = f.open();
    assert_eq!(
        f.logs().len(),
        1,
        "the next open removes what the last one could not"
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"v"[..]));
}
