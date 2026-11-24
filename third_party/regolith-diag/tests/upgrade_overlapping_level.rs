//! A database regolith 0.1.9 left with two tables of overlapping key ranges
//! in one level opens on this version: the open moves every level from
//! level 1 down to the overlapping one into level 0, oldest level first,
//! warns once, and every read returns the newest version.
//!
//! The database is `tests/fixtures/v0_1_9_overlap`, written by the 0.1.9
//! release; `common/overlap_fixture.rs` says how, and what it holds.

// Native-only, like every test that touches the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::keys::Keys;
use common::log_capture::warnings_of;
use common::overlap_fixture::{
    self as fixture, MOVED, assert_db_reads, assert_reads, moved, value,
};
use regolith::{Db, Error, FixedLengthPrefix, Options, Statistics, Ticker};

fn opts() -> Options {
    fixture::with_operator(Options::default())
}

fn files_at(db: &Db, level: usize) -> u64 {
    db.get_int_property(&format!("regolith.num-files-at-level{level}"))
        .unwrap()
}

fn levels(db: &Db) -> Vec<u64> {
    (0..7).map(|level| files_at(db, level)).collect()
}

/// The lines among `warnings` that report the move.
fn move_lines(warnings: &[String]) -> Vec<&String> {
    warnings
        .iter()
        .filter(|line| line.contains("overlapping key ranges"))
        .collect()
}

/// The headline: the database opens, the move is reported once, by one
/// line and one ticker, and every read returns the newest version.
#[test]
fn a_0_1_9_database_with_an_overlapping_level_opens_and_reads_its_newest_versions() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();

    let stats = Arc::new(Statistics::new());
    let (db, warnings) = warnings_of(|| {
        Db::open(&db_dir, opts().statistics(Some(Arc::clone(&stats))))
            .expect("a 0.1.9 database with an overlapping level opens")
    });
    assert_eq!(moved(&db), MOVED);
    assert_eq!(stats.get_ticker(Ticker::ManifestTablesDemoted), MOVED);
    let lines = move_lines(&warnings);
    assert_eq!(
        lines.len(),
        1,
        "one line for the move, not one per table: {warnings:?}"
    );
    assert!(lines[0].contains("no action is needed"), "{}", lines[0]);
    // Every table below level 0 joined the one level 0 held.
    assert_eq!(levels(&db), [MOVED + 1, 0, 0, 0, 0, 0, 0]);
    assert_db_reads("the first open", &db, &expected);
    db.close().unwrap();
    drop(db);

    // The move happens once: the next open finds nothing to move. The
    // close wrote what the first open recovered as one more table.
    let stats = Arc::new(Statistics::new());
    let (db, warnings) = warnings_of(|| {
        Db::open(&db_dir, opts().statistics(Some(Arc::clone(&stats)))).expect("it reopens")
    });
    assert_eq!(moved(&db), 0);
    assert_eq!(stats.get_ticker(Ticker::ManifestTablesDemoted), 0);
    assert!(move_lines(&warnings).is_empty(), "{warnings:?}");
    assert_eq!(levels(&db), [MOVED + 2, 0, 0, 0, 0, 0, 0]);
    assert_db_reads("the second open", &db, &expected);
}

/// What 0.1.9 could not serve from the level it read first: a key older
/// than the ingest's range tombstone, in a level above it, reads as
/// deleted by every reader, and a scan no longer fails.
#[test]
fn a_range_tombstone_of_a_moved_table_deletes_the_older_key_above_it() {
    let (_tmp, db_dir) = fixture::fresh();
    let db = Db::open(&db_dir, opts()).unwrap();
    assert_eq!(db.get(b"b").unwrap(), None);
    assert_eq!(db.multi_get(&[b"b"]).unwrap(), [None]);
    let scanned = db.scan(None, None).expect("a scan of the moved tables");
    assert!(scanned.iter().all(|(key, _)| key != b"b"));
    // A key the tombstone covers, written after it, is untouched.
    assert_eq!(db.get(b"d").unwrap().as_deref(), Some(&value("new")[..]));
}

/// A compaction must not change a read. Of the tables the open moved,
/// only the ingested one, with its range tombstone, and the older table
/// beside it meet `[a, a0)`; the table holding `b`, which that tombstone
/// deletes, does not. Compacting that range must not take the tombstone
/// below `b` and bring `b` back.
#[test]
fn a_compaction_that_takes_the_range_tombstone_away_changes_no_read() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let db = Db::open(&db_dir, opts()).unwrap();
    db.compact_range(Some(b"a"), Some(b"a0")).wait().unwrap();
    assert!(files_at(&db, 1) > 0, "the ingested table left level 0");
    assert_db_reads("after compacting [a, a0)", &db, &expected);

    // The same once the memtable holding what the open recovered is a
    // table, and once everything is one level.
    db.flush().unwrap();
    assert_db_reads("after a flush", &db, &expected);
    db.compact_range(None, None).wait().unwrap();
    assert_db_reads("after compacting everything", &db, &expected);
    db.close().unwrap();
    drop(db);
    let db = Db::open(&db_dir, opts()).unwrap();
    assert_db_reads("after a reopen", &db, &expected);
}

/// A snapshot taken on the upgraded database keeps reading the newest
/// versions as of its sequence, through later writes, a flush and a
/// compaction of every level.
#[test]
fn a_snapshot_reads_the_newest_versions_through_later_writes_and_compactions() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let db = Db::open(&db_dir, opts()).unwrap();
    let snapshot = db.snapshot();

    let mut later = expected.clone();
    db.put(b"b", b"back").unwrap();
    later.insert(b"b".to_vec(), b"back".to_vec());
    db.delete(b"d").unwrap();
    later.remove(b"d".as_slice());
    db.merge(b"j2", b"5").unwrap();
    later.insert(b"j2".to_vec(), b"12345".to_vec());
    db.delete_range(b"m", b"o").unwrap();
    later.remove(b"m".as_slice());
    later.remove(b"n".as_slice());

    let assert_snapshot = |when: &str| {
        assert_reads(
            when,
            |key| snapshot.get(key).unwrap().map(|v| v.to_vec()),
            |keys| {
                snapshot
                    .multi_get(keys)
                    .unwrap()
                    .into_iter()
                    .map(|v| v.map(|v| v.to_vec()))
                    .collect()
            },
            || snapshot.scan(None, None).unwrap(),
            &expected,
        );
    };
    assert_snapshot("the snapshot, before a flush");
    assert_db_reads("the database, before a flush", &db, &later);
    db.flush().unwrap();
    db.compact_range(None, None).wait().unwrap();
    assert_snapshot("the snapshot, after compacting everything");
    assert_db_reads("the database, after compacting everything", &db, &later);
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

/// A read-only open moves the tables in its own view, reads the same, and
/// writes nothing: the manifest keeps the overlap, so every such open moves
/// them and says so, until a read-write open records the move.
#[test]
fn a_read_only_open_reads_the_same_and_leaves_the_manifest_alone() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let before = files_of(&db_dir);

    for _ in 0..2 {
        let stats = Arc::new(Statistics::new());
        let (db, warnings) = warnings_of(|| {
            Db::open_read_only(&db_dir, opts().statistics(Some(Arc::clone(&stats))))
                .expect("it opens read-only")
        });
        assert_eq!(moved(&db), MOVED);
        assert_eq!(stats.get_ticker(Ticker::ManifestTablesDemoted), MOVED);
        let lines = move_lines(&warnings);
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
    assert_eq!(moved(&db), MOVED);
    db.close().unwrap();
    drop(db);
    let db = Db::open_read_only(&db_dir, opts()).unwrap();
    assert_eq!(moved(&db), 0, "the read-write open recorded the move");
    assert_db_reads("read-only, after the move is recorded", &db, &expected);
}

/// The upgrade and the first use of a key provider in one open: the move
/// is recorded in a manifest sealed from then on, and the database reads
/// the same with the key and not at all without it.
#[test]
fn the_move_is_recorded_sealed_when_the_upgrade_turns_encryption_on() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let sealed = || opts().key_provider(Keys::new(&[1]));

    let db = Db::open(&db_dir, sealed()).expect("it opens with a key provider");
    assert_eq!(moved(&db), MOVED);
    // The open that moved the tables also left the manifest sealed: the
    // byte after its magic is the sealed format's.
    assert_eq!(
        fs::read(db_dir.join("MANIFEST")).unwrap()[..8],
        *b"REGOMAN\x02",
        "the manifest that records the move is not sealed"
    );
    assert_db_reads("the first sealed open", &db, &expected);
    db.put(b"s", b"sealed").unwrap();
    db.close().unwrap();
    drop(db);

    assert!(
        matches!(
            Db::open_read_only(&db_dir, opts()),
            Err(Error::KeyProviderRequired)
        ),
        "the manifest that records the move is readable without the key"
    );
    let mut with_s = expected.clone();
    with_s.insert(b"s".to_vec(), b"sealed".to_vec());
    let db = Db::open(&db_dir, sealed()).expect("it reopens with the key");
    assert_eq!(moved(&db), 0);
    assert_db_reads_and(&db, b"s", "the second sealed open", &with_s);
    // Every table rewritten under the key still reads the same.
    db.compact_range(None, None).wait().unwrap();
    assert_db_reads_and(&db, b"s", "sealed, after compacting everything", &with_s);
}

/// [`assert_db_reads`], and `extra`, a key the fixture does not name.
fn assert_db_reads_and(
    db: &Db,
    extra: &[u8],
    read: &str,
    expected: &std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
) {
    assert_db_reads(read, db, expected);
    assert_eq!(
        db.get(extra).unwrap().map(|v| v.to_vec()).as_ref(),
        expected.get(extra),
        "{read}"
    );
}

/// The keys a prefix seek yields, in order.
fn seek_prefix(db: &Db, prefix: &[u8]) -> Vec<Vec<u8>> {
    let mut iter = db.iter();
    iter.seek_prefix(prefix);
    let mut keys = Vec::new();
    while iter.valid() {
        keys.push(iter.key().unwrap().to_vec());
        iter.next();
    }
    iter.status().unwrap();
    keys
}

/// A prefix seek returns what a full scan returns for that prefix, on a
/// database opened with a prefix extractor it was not written with. The
/// tables 0.1.9 wrote carry no prefix filter and are read whole; the tables
/// this version writes afterwards carry one, built over their keys, and a
/// range tombstone in a table its filter rules out is still in force: `b`,
/// which the moved table's tombstone deletes, stays deleted at every stage.
#[test]
fn a_prefix_seek_over_the_moved_tables_returns_what_a_full_scan_returns() {
    let (_tmp, db_dir) = fixture::fresh();
    let expected = fixture::state();
    let prefixed = || opts().prefix_extractor(Some(Arc::new(FixedLengthPrefix(1))));
    let assert_seeks = |db: &Db, when: &str| {
        let scanned: Vec<Vec<u8>> = db
            .scan(None, None)
            .unwrap()
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(
            scanned,
            expected.keys().cloned().collect::<Vec<_>>(),
            "{when}: scan"
        );
        for first in b'a'..=b'r' {
            let want: Vec<Vec<u8>> = scanned
                .iter()
                .filter(|key| key.first() == Some(&first))
                .cloned()
                .collect();
            assert_eq!(
                seek_prefix(db, &[first]),
                want,
                "{when}: prefix {}",
                first as char
            );
        }
    };

    let db = Db::open(&db_dir, prefixed()).unwrap();
    assert_eq!(moved(&db), MOVED);
    assert_seeks(&db, "the first open");
    // The ingested table, with its range tombstone, leaves level 0 in a
    // table this version writes.
    db.compact_range(Some(b"a"), Some(b"a0")).wait().unwrap();
    assert_seeks(&db, "after compacting [a, a0)");
    // What the open recovered, the copy of that tombstone with it, becomes
    // a table too.
    db.flush().unwrap();
    assert_seeks(&db, "after a flush");
    db.compact_range(None, None).wait().unwrap();
    assert_seeks(&db, "after compacting everything");
    db.close().unwrap();
    drop(db);
    let db = Db::open(&db_dir, prefixed()).unwrap();
    assert_seeks(&db, "after a reopen");
}

/// The upgraded database takes writes like any other, and keeps them.
#[test]
fn the_upgraded_database_takes_writes_and_keeps_them_across_a_reopen() {
    let (_tmp, db_dir) = fixture::fresh();
    let mut expected = fixture::state();
    {
        let db = Db::open(&db_dir, opts()).unwrap();
        // `c` was deleted by the ingest's range tombstone; a put brings
        // it back, newer than the tombstone.
        db.put(b"c", b"again").unwrap();
        expected.insert(b"c".to_vec(), b"again".to_vec());
        db.delete(b"q").unwrap();
        expected.remove(b"q".as_slice());
        db.merge(b"a1", b"z").unwrap();
        expected.insert(b"a1".to_vec(), b"yz".to_vec());
        assert_db_reads("before the close", &db, &expected);
        db.close().unwrap();
    }
    let db = Db::open(&db_dir, opts()).unwrap();
    assert_db_reads("after the reopen", &db, &expected);
}
