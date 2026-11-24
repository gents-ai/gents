//! A database regolith 0.1.9 left with an ingested table in its bottom
//! level, whose range delete is newer than keys it covers in a level every
//! read consults first, opens on this version with every read right: the
//! open copies that range delete in front of every key, warns once, and
//! records that it checked, so no later open checks again (E37).
//!
//! The database is `tests/fixtures/v0_1_9_buried_delete`, written by the
//! 0.1.9 release; `common/buried_fixture.rs` says how, and what it holds.
//! 0.1.9 itself returned `c` from a point read while its scan left it out.

// Native-only, like every test that touches the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::buried_fixture::{self as fixture, COPIED, assert_db_reads, copied};
use common::keys::Keys;
use common::log_capture::{lines_with, warnings_of};
use common::overlap_fixture::{value, with_operator};
use regolith::{Db, Error, Options, Statistics, Ticker};

fn opts() -> Options {
    with_operator(Options::default())
}

/// What every copy line says.
const COPY_LINE: &str = "copied range deletes an earlier release may have left behind";

/// The headline: the open copies the range delete, says so once, by one
/// line, the ticker and the property, and every read is right; the next
/// open checks nothing.
#[test]
fn a_0_1_9_database_with_a_range_delete_below_older_keys_reads_them_deleted() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();

    let stats = Arc::new(Statistics::new());
    let (db, warnings) = warnings_of(|| {
        Db::open(&db_dir, opts().statistics(Some(Arc::clone(&stats))))
            .expect("a 0.1.9 database with a range delete below older keys opens")
    });
    assert_eq!(copied(&db), COPIED);
    assert_eq!(stats.get_ticker(Ticker::ManifestRangeDeletesCopied), COPIED);
    assert_eq!(
        db.get_int_property("regolith.manifest.tables_demoted"),
        Some(0),
        "no level overlaps: nothing is moved"
    );
    let lines = lines_with(&warnings, COPY_LINE);
    assert_eq!(lines.len(), 1, "one line for the copies: {warnings:?}");
    assert!(lines[0].contains("no action is needed"), "{}", lines[0]);
    // What 0.1.9 answered wrong: a point read of `c`, and of `a1`, whose
    // first operand the range delete deletes.
    assert_eq!(db.get(b"c").unwrap(), None);
    assert_eq!(db.get(b"a1").unwrap().as_deref(), Some(&b"y"[..]));
    assert_db_reads("the first open", &db, &expected);
    db.close().unwrap();
    drop(db);

    let stats = Arc::new(Statistics::new());
    let (db, warnings) = warnings_of(|| {
        Db::open(&db_dir, opts().statistics(Some(Arc::clone(&stats)))).expect("it reopens")
    });
    assert_eq!(copied(&db), 0);
    assert_eq!(stats.get_ticker(Ticker::ManifestRangeDeletesCopied), 0);
    assert!(lines_with(&warnings, COPY_LINE).is_empty(), "{warnings:?}");
    assert_db_reads("the second open", &db, &expected);
}

/// The range delete's own table and the one holding `c` stay where they
/// are, and compactions of every shape, a flush, later writes and a
/// snapshot change no read.
#[test]
fn compactions_writes_and_a_snapshot_keep_the_deleted_keys_deleted() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let db = Db::open(&db_dir, opts()).unwrap();
    let snapshot = db.snapshot();

    let mut later = expected.clone();
    db.put(b"b", b"after").unwrap();
    later.insert(b"b".to_vec(), b"after".to_vec());
    db.merge(b"a1", b"z").unwrap();
    later.insert(b"a1".to_vec(), b"yz".to_vec());

    // A pass over the bottom level's tables alone, then everything.
    db.compact_range(Some(b"w"), Some(b"z")).wait().unwrap();
    assert_db_reads("after compacting [w, z]", &db, &later);
    db.flush().unwrap();
    assert_db_reads("after a flush", &db, &later);
    db.compact_range(None, None).wait().unwrap();
    assert_db_reads("after compacting everything", &db, &later);
    assert_eq!(snapshot.get(b"c").unwrap(), None);
    assert_eq!(snapshot.get(b"a1").unwrap().as_deref(), Some(&b"y"[..]));
    assert_eq!(snapshot.get(b"b").unwrap(), None);
    drop(snapshot);
    db.close().unwrap();
    drop(db);
    let db = Db::open(&db_dir, opts()).unwrap();
    assert_db_reads("after a reopen", &db, &later);
}

/// Every file under `dir` with its bytes, the lock file aside.
fn files_of(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name != "LOCK") {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

/// A read-only open copies the range delete in its own view, reads right,
/// says so, and writes nothing; until a read-write open records the check,
/// every read-only open does the same.
#[test]
fn a_read_only_open_reads_right_and_writes_nothing() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let before = files_of(&db_dir);
    for _ in 0..2 {
        let stats = Arc::new(Statistics::new());
        let (db, warnings) = warnings_of(|| {
            Db::open_read_only(&db_dir, opts().statistics(Some(Arc::clone(&stats))))
                .expect("it opens read-only")
        });
        assert_eq!(copied(&db), COPIED);
        assert_eq!(stats.get_ticker(Ticker::ManifestRangeDeletesCopied), COPIED);
        let lines = lines_with(&warnings, COPY_LINE);
        assert_eq!(lines.len(), 1, "{warnings:?}");
        assert!(
            lines[0].contains("open the database for writing once"),
            "{}",
            lines[0]
        );
        assert_db_reads("a read-only open", &db, &expected);
        drop(db);
        assert_eq!(files_of(&db_dir), before, "a read-only open wrote a file");
    }
    let db = Db::open(&db_dir, opts()).unwrap();
    assert_eq!(copied(&db), COPIED);
    db.close().unwrap();
    drop(db);
    let db = Db::open_read_only(&db_dir, opts()).unwrap();
    assert_eq!(copied(&db), 0, "the read-write open recorded the check");
    assert_db_reads("read-only, after the check is recorded", &db, &expected);
}

/// The first open with a key provider seals the manifest with the batch
/// that records the check; the database reads the same with the key and
/// does not open without it.
#[test]
fn the_check_is_recorded_sealed_when_the_upgrade_turns_encryption_on() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let sealed = || opts().key_provider(Keys::new(&[1]));
    let db = Db::open(&db_dir, sealed()).expect("it opens with a key provider");
    assert_eq!(copied(&db), COPIED);
    assert_eq!(
        fs::read(db_dir.join("MANIFEST")).unwrap()[..8],
        *b"REGOMAN\x02",
        "the manifest that records the check is not sealed"
    );
    assert_db_reads("the first sealed open", &db, &expected);
    db.close().unwrap();
    drop(db);
    assert!(matches!(
        Db::open_read_only(&db_dir, opts()),
        Err(Error::KeyProviderRequired)
    ));
    let db = Db::open(&db_dir, sealed()).unwrap();
    assert_eq!(copied(&db), 0);
    assert_db_reads("the second sealed open", &db, &expected);
}

/// A backup of the upgraded database restores a database that needs no
/// check: the copy is in its tables, and its manifest records the check.
#[test]
fn a_backup_of_the_upgraded_database_restores_checked() {
    let (tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let db = Db::open(&db_dir, opts()).unwrap();
    let mut backups = regolith::BackupEngine::open(tmp.path().join("backups")).unwrap();
    let id = backups.create_backup(&db).unwrap();
    drop(db);
    let restored = tmp.path().join("restored");
    backups.restore(id, &restored, None).unwrap();
    let db = Db::open(&restored, opts()).unwrap();
    assert_eq!(copied(&db), 0);
    assert_db_reads("the restored database", &db, &expected);
    assert_eq!(db.get(b"d").unwrap().as_deref(), Some(&value("new")[..]));
}
