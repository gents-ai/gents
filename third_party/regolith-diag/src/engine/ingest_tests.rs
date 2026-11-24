//! Ingest without a rewrite (D48): the copy holds no lock, only a memtable
//! holding a key of the file's range is flushed, the table lands where
//! LsmOrder.tla places it, and every read, snapshot, validation, compaction
//! and reopen sees its entries at the sequence the manifest records.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

use super::while_next_ingest_stages;
use crate::column_family::{DEFAULT_CF_ID, prefix_key};
use crate::engine::internal_key::{VALUE_TYPE_VALUE, decode_internal_key, encode_internal_key};
use crate::engine::manifest::MAX_LEVELS;
use crate::engine::sstable::SsTableWriter;
use crate::options::CompressionType;
use crate::{
    Db, IngestOptions, IsolationLevel, OptimisticTransactionDb, Options, SstFileWriter,
    TransactionError, TxnOptions,
};

fn options() -> Options {
    Options::default().max_background_compactions(0)
}

/// An ingest that does not refuse a pinned snapshot, which several tests
/// hold on purpose.
fn ingest_opts() -> IngestOptions {
    IngestOptions {
        snapshot_consistency: false,
        ..IngestOptions::default()
    }
}

/// An external table, as `SstFileWriter` writes it, holding `entries`.
fn source(dir: &Path, name: &str, entries: &[(&[u8], &[u8])]) -> PathBuf {
    let path = dir.join(name);
    let mut writer = SstFileWriter::create(&path, &options()).unwrap();
    for (key, value) in entries {
        writer.put(key, value).unwrap();
    }
    writer.finish().unwrap();
    path
}

fn files_at(db: &Db, level: usize) -> u64 {
    db.get_int_property(&format!("regolith.num-files-at-level{level}"))
        .unwrap()
}

/// The recorded sequence of every table in the current version, by level.
fn recorded_seqs(db: &Db) -> Vec<(usize, Option<u64>)> {
    db.engine
        .published_version()
        .levels
        .iter()
        .enumerate()
        .flat_map(|(level, files)| files.iter().map(move |f| (level, f.meta.global_seq)))
        .collect()
}

/// Polls `done` with a bounded backoff until it holds or `deadline` passes.
fn within(deadline: Duration, mut done: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    let mut backoff = Duration::from_micros(100);
    while !done() {
        if start.elapsed() >= deadline {
            return false;
        }
        thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(20));
    }
    true
}

#[test]
fn a_writer_commits_while_an_ingest_copies_its_file() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path().join("db"), options()).unwrap());
    let path = source(dir.path(), "s.sst", &[(b"k", b"new")]);

    // The writer is joined only once the ingest has returned: a writer
    // held back by the ingest cannot finish while the hook runs.
    let committed = Rc::new(Cell::new(false));
    let writer = Rc::new(RefCell::new(None));
    let (held, seen, handle) = (Arc::clone(&db), Rc::clone(&committed), Rc::clone(&writer));
    while_next_ingest_stages(move || {
        let spawned = {
            let db = Arc::clone(&held);
            thread::spawn(move || db.put(b"w", b"1"))
        };
        seen.set(within(Duration::from_secs(10), || spawned.is_finished()));
        *handle.borrow_mut() = Some(spawned);
    });
    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();
    let spawned = writer.borrow_mut().take().expect("the hook ran");
    spawned.join().unwrap().unwrap();

    assert!(
        committed.get(),
        "a write waited for an ingest to copy its file"
    );
    assert_eq!(db.get(b"w").unwrap().as_deref(), Some(&b"1"[..]));
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn a_memtable_holding_a_key_of_the_range_is_flushed_before_the_table_installs() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"k", b"old").unwrap();
    let before = db.snapshot();
    let path = source(dir.path(), "s.sst", &[(b"j", b"new"), (b"k", b"new")]);

    db.ingest_external_files(&[path], ingest_opts())
        .wait()
        .unwrap();

    assert_eq!(
        db.get_int_property("regolith.num-entries-active-mem-table"),
        Some(0),
        "the memtable holding `k` was flushed"
    );
    // The flushed table holds `k`, so the ingested one goes in front of it.
    assert_eq!(files_at(&db, 0), 2);
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(before.get(b"k").unwrap().as_deref(), Some(&b"old"[..]));
}

#[test]
fn a_memtable_holding_no_key_of_the_range_is_not_flushed() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"a", b"1").unwrap();
    db.put(b"z", b"1").unwrap();
    let path = source(dir.path(), "s.sst", &[(b"m", b"new"), (b"n", b"new")]);

    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();

    assert_eq!(
        db.get_int_property("regolith.num-entries-active-mem-table"),
        Some(2),
        "a memtable holding no key of the range was flushed for the ingest"
    );
    assert_eq!(files_at(&db, 0), 0);
    assert_eq!(
        files_at(&db, MAX_LEVELS - 1),
        1,
        "an empty tree takes it at the bottom"
    );
    let expected: [(&[u8], &[u8]); 4] =
        [(b"a", b"1"), (b"z", b"1"), (b"m", b"new"), (b"n", b"new")];
    for (key, value) in expected {
        assert_eq!(db.get(key).unwrap().as_deref(), Some(value));
    }
}

#[test]
fn a_range_tombstone_in_a_memtable_reaching_the_range_counts_as_a_key_of_it() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"z", b"1").unwrap();
    db.delete_range(b"a", b"c").unwrap();
    let path = source(dir.path(), "s.sst", &[(b"b", b"new")]);

    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();

    assert_eq!(
        db.get_int_property("regolith.num-entries-active-mem-table"),
        Some(0),
        "the memtable whose tombstone reaches `b` was flushed"
    );
    assert_eq!(files_at(&db, 0), 2);
    assert_eq!(db.get(b"b").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"z").unwrap().as_deref(), Some(&b"1"[..]));
}

#[test]
fn an_l0_table_whose_range_meets_the_file_but_holds_none_of_its_keys_lets_it_go_deeper() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"a", b"1").unwrap();
    db.put(b"z", b"1").unwrap();
    db.flush().unwrap();
    assert_eq!(files_at(&db, 0), 1);

    // [a, z] meets [m, n], but no key of the L0 table lies in it.
    let clear = source(dir.path(), "clear.sst", &[(b"m", b"new"), (b"n", b"new")]);
    db.ingest_external_files(&[clear], IngestOptions::default())
        .wait()
        .unwrap();
    assert_eq!(files_at(&db, 0), 1);
    assert_eq!(files_at(&db, MAX_LEVELS - 1), 1);

    // An L0 table holding a key of the range keeps the next one in L0.
    db.put(b"p", b"old").unwrap();
    db.flush().unwrap();
    let held = source(dir.path(), "held.sst", &[(b"o", b"new"), (b"p", b"new")]);
    db.ingest_external_files(&[held], IngestOptions::default())
        .wait()
        .unwrap();
    assert_eq!(files_at(&db, 0), 3);
    assert_eq!(db.get(b"p").unwrap().as_deref(), Some(&b"new"[..]));
    assert_eq!(db.get(b"m").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn reads_iterators_and_snapshots_see_the_recorded_sequence() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    let keys: Vec<Vec<u8>> = (0..300u32)
        .map(|i| format!("k{i:04}").into_bytes())
        .collect();
    for key in &keys {
        db.put(key, b"old").unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    let before = db.snapshot();

    let entries: Vec<(&[u8], &[u8])> = keys
        .iter()
        .step_by(2)
        .map(|k| (k.as_slice(), &b"new"[..]))
        .collect();
    let path = source(dir.path(), "s.sst", &entries);
    db.ingest_external_files(&[path], ingest_opts())
        .wait()
        .unwrap();
    assert_eq!(
        recorded_seqs(&db),
        vec![
            (MAX_LEVELS - 2, Some(before.sequence() + 1)),
            (MAX_LEVELS - 1, None)
        ],
        "the table lands above the old data, recording the sequence it drew"
    );

    let expect = |i: usize| -> &[u8] { if i.is_multiple_of(2) { b"new" } else { b"old" } };
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(db.get(key).unwrap().as_deref(), Some(expect(i)));
        assert_eq!(db.get_size(key).unwrap(), Some(3));
        assert_eq!(before.get(key).unwrap().as_deref(), Some(&b"old"[..]));
    }
    let refs: Vec<&[u8]> = keys.iter().map(Vec::as_slice).collect();
    let got = db.multi_get(&refs).unwrap();
    for (i, value) in got.iter().enumerate() {
        assert_eq!(value.as_deref(), Some(expect(i)));
    }

    let mut it = db.iter();
    it.seek_to_first();
    for (i, key) in keys.iter().enumerate() {
        assert_eq!(
            (it.key(), it.value()),
            (Some(key.as_slice()), Some(expect(i)))
        );
        it.next();
    }
    assert!(!it.valid());
    it.seek_to_last();
    for (i, key) in keys.iter().enumerate().rev() {
        assert_eq!(
            (it.key(), it.value()),
            (Some(key.as_slice()), Some(expect(i)))
        );
        it.prev();
    }
    for i in [0usize, 1, 150, 151, 298, 299] {
        it.seek(&keys[i]);
        assert_eq!(it.value(), Some(expect(i)));
        it.seek_for_prev(&keys[i]);
        assert_eq!(it.value(), Some(expect(i)));
    }
    let mut old = before.iter();
    old.seek_to_first();
    let mut seen = 0;
    while old.valid() {
        assert_eq!(old.value(), Some(&b"old"[..]));
        seen += 1;
        old.next();
    }
    assert_eq!(seen, keys.len());
}

/// A source whose own entries sit at `stored_seq`, written straight through
/// the engine's writer with blocks a few entries wide, flat or partitioned.
fn raw_source(path: &Path, keys: &[Vec<u8>], stored_seq: u64, partitioned: bool) {
    let mut writer =
        SsTableWriter::new(path, 64, 10, CompressionType::None, None, partitioned, 64).unwrap();
    for key in keys {
        let internal = encode_internal_key(
            &prefix_key(DEFAULT_CF_ID, key),
            stored_seq,
            VALUE_TYPE_VALUE,
        );
        writer.add(&internal, b"new").unwrap();
    }
    writer.finish().unwrap().unwrap();
}

#[test]
fn a_table_stored_at_other_sequences_reads_at_the_recorded_one() {
    for partitioned in [false, true] {
        for stored_seq in [0, 7, 1_000_000] {
            let dir = TempDir::new().unwrap();
            let db = Db::open(dir.path().join("db"), options()).unwrap();
            let keys: Vec<Vec<u8>> = (0..60u32)
                .map(|i| format!("k{i:03}").into_bytes())
                .collect();
            for key in &keys {
                db.put(key, b"old").unwrap();
            }
            db.flush().unwrap();
            let before = db.snapshot();
            let path = dir.path().join("raw.sst");
            raw_source(&path, &keys, stored_seq, partitioned);
            db.ingest_external_files(&[path], ingest_opts())
                .wait()
                .unwrap();
            let case = format!("partitioned {partitioned}, stored at {stored_seq}");

            for key in &keys {
                assert_eq!(db.get(key).unwrap().as_deref(), Some(&b"new"[..]), "{case}");
                assert_eq!(
                    before.get(key).unwrap().as_deref(),
                    Some(&b"old"[..]),
                    "{case}"
                );
            }
            let mut it = db.iter();
            for key in &keys {
                it.seek(key);
                assert_eq!(it.value(), Some(&b"new"[..]), "{case}");
                it.seek_for_prev(key);
                assert_eq!(it.value(), Some(&b"new"[..]), "{case}");
            }
            let mut it = before.iter();
            for key in &keys {
                it.seek(key);
                assert_eq!(it.value(), Some(&b"old"[..]), "{case}");
            }
        }
    }
}

#[test]
fn a_transaction_that_read_a_key_before_an_ingest_replaced_it_conflicts() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path().join("db"), options()).unwrap();
    db.db().put(b"k", b"old").unwrap();
    db.db().compact_range(None, None).wait().unwrap();

    let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    assert_eq!(txn.get(b"k").unwrap().as_deref(), Some(&b"old"[..]));
    let path = source(dir.path(), "s.sst", &[(b"k", b"new")]);
    db.db()
        .ingest_external_files(&[path], ingest_opts())
        .wait()
        .unwrap();
    txn.put(b"k", b"old, updated").unwrap();
    match txn.commit() {
        Err(TransactionError::Conflict(_)) => {}
        other => panic!("a transaction committed over an ingest it never saw: {other:?}"),
    }

    let later = db.begin(&TxnOptions::new().isolation(IsolationLevel::SnapshotIsolation));
    assert_eq!(later.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
    later.put(b"k", b"newer").unwrap();
    later.commit().unwrap();
    assert_eq!(db.db().get(b"k").unwrap().as_deref(), Some(&b"newer"[..]));
}

#[test]
fn compaction_writes_the_recorded_sequence_into_its_output_and_records_none() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"k", b"old").unwrap();
    db.flush().unwrap();
    let path = source(dir.path(), "s.sst", &[(b"k", b"new"), (b"l", b"new")]);
    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();
    let seq = db.latest_sequence();
    assert_eq!(recorded_seqs(&db), vec![(0, None), (0, Some(seq))]);

    db.compact_range(None, None).wait().unwrap();

    let version = db.engine.published_version();
    let mut seen = Vec::new();
    for file in version.levels.iter().flatten() {
        assert_eq!(
            file.meta.global_seq, None,
            "a compaction output records no sequence"
        );
        for (ik, value) in file.reader.iter_internal(&db.engine.cache).unwrap() {
            let (user_key, entry_seq, _) = decode_internal_key(&ik);
            seen.push((user_key.to_vec(), entry_seq, value));
        }
    }
    assert_eq!(
        seen,
        vec![
            (prefix_key(DEFAULT_CF_ID, b"k"), seq, b"new".to_vec()),
            (prefix_key(DEFAULT_CF_ID, b"l"), seq, b"new".to_vec()),
        ],
        "the output carries the recorded sequence in its entries, and the older `k` is gone"
    );
    drop(db);
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
}

#[test]
fn an_ingested_table_reopens_and_checkpoints_at_its_recorded_sequence() {
    let dir = TempDir::new().unwrap();
    let db_path = dir.path().join("db");
    let db = Db::open(&db_path, options()).unwrap();
    db.put(b"k", b"old").unwrap();
    db.compact_range(None, None).wait().unwrap();
    let path = source(dir.path(), "s.sst", &[(b"k", b"new")]);
    db.ingest_external_files(&[path], IngestOptions::default())
        .wait()
        .unwrap();
    let seq = db.latest_sequence();
    db.checkpoint(dir.path().join("checkpoint")).wait().unwrap();
    db.close().unwrap();
    drop(db);

    for reopened in [db_path, dir.path().join("checkpoint")] {
        let db = Db::open(&reopened, options()).unwrap();
        assert_eq!(
            recorded_seqs(&db),
            vec![(MAX_LEVELS - 2, Some(seq)), (MAX_LEVELS - 1, None)]
        );
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
        // The reopened counter starts above the recorded sequence, so a
        // write after the reopen is the newest version.
        db.put(b"k", b"after").unwrap();
        assert!(db.latest_sequence() > seq);
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"after"[..]));
    }
}

#[test]
fn a_source_holding_a_key_twice_is_refused_and_leaves_nothing_behind() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    let path = dir.path().join("dup.sst");
    let mut writer =
        SsTableWriter::new(&path, 4096, 10, CompressionType::None, None, false, 4096).unwrap();
    let key = prefix_key(DEFAULT_CF_ID, b"k");
    writer
        .add(&encode_internal_key(&key, 5, VALUE_TYPE_VALUE), b"a")
        .unwrap();
    writer
        .add(&encode_internal_key(&key, 3, VALUE_TYPE_VALUE), b"b")
        .unwrap();
    writer.finish().unwrap().unwrap();

    match db
        .ingest_external_files(&[path], IngestOptions::default())
        .wait()
    {
        Err(crate::Error::InvalidArgument(message)) => {
            assert!(
                message.contains("more than one entry for a key"),
                "{message}"
            )
        }
        other => panic!("a file holding a key twice was not refused: {other:?}"),
    }
    let tables = std::fs::read_dir(dir.path().join("db").join("sst"))
        .unwrap()
        .count();
    assert_eq!(
        tables, 0,
        "the refused file was left in the table directory"
    );
    assert_eq!(db.get(b"k").unwrap(), None);
}

#[test]
fn a_moved_file_is_linked_and_its_source_removed_and_a_copied_one_is_independent() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();

    let moved = source(dir.path(), "moved.sst", &[(b"m", b"moved")]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let (sst_dir, source_path) = (dir.path().join("db").join("sst"), moved.clone());
        while_next_ingest_stages(move || {
            let staged = std::fs::read_dir(&sst_dir)
                .unwrap()
                .next()
                .unwrap()
                .unwrap();
            assert_eq!(
                staged.metadata().unwrap().ino(),
                std::fs::metadata(&source_path).unwrap().ino(),
                "a moved file is staged as a link to its source"
            );
        });
    }
    db.ingest_external_files(
        std::slice::from_ref(&moved),
        IngestOptions {
            move_files: true,
            ..IngestOptions::default()
        },
    )
    .wait()
    .unwrap();
    assert!(!moved.exists(), "a moved source is removed once installed");

    // A copied source can be rewritten in place afterwards without
    // touching the table, which is why only a move is linked.
    let copied = source(dir.path(), "copied.sst", &[(b"c", b"copied")]);
    db.ingest_external_files(std::slice::from_ref(&copied), IngestOptions::default())
        .wait()
        .unwrap();
    assert!(copied.exists());
    std::fs::write(&copied, b"rewritten in place").unwrap();

    drop(db);
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    assert_eq!(db.get(b"m").unwrap().as_deref(), Some(&b"moved"[..]));
    assert_eq!(db.get(b"c").unwrap().as_deref(), Some(&b"copied"[..]));
}

#[test]
fn a_refused_install_unlinks_the_staged_file() {
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path().join("db"), options()).unwrap();
    db.put(b"k", b"old").unwrap();
    db.flush().unwrap();
    let path = source(dir.path(), "s.sst", &[(b"k", b"new")]);
    let tables = || {
        std::fs::read_dir(dir.path().join("db").join("sst"))
            .unwrap()
            .count()
    };
    assert_eq!(tables(), 1);

    let behind = IngestOptions {
        ingest_behind: true,
        ..IngestOptions::default()
    };
    assert!(db.ingest_external_files(&[path], behind).wait().is_err());
    assert_eq!(
        tables(),
        1,
        "the staged file was left in the table directory"
    );
    assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"old"[..]));
}

#[test]
fn concurrent_ingests_of_files_with_the_same_layout_read_their_own_bytes() {
    let dir = TempDir::new().unwrap();
    let db = Arc::new(Db::open(dir.path().join("db"), options()).unwrap());
    let threads: Vec<_> = (0..4u32)
        .map(|t| {
            let db = Arc::clone(&db);
            let staging = dir.path().to_path_buf();
            thread::spawn(move || {
                for round in 0..8u32 {
                    let keys: Vec<Vec<u8>> = (0..40u32)
                        .map(|i| format!("t{t}r{round:02}k{i:03}").into_bytes())
                        .collect();
                    // Same key and value lengths in every file: their
                    // blocks sit at the same offsets.
                    let value = format!("v{t}{round:02}");
                    let entries: Vec<(&[u8], &[u8])> = keys
                        .iter()
                        .map(|k| (k.as_slice(), value.as_bytes()))
                        .collect();
                    let path = source(&staging, &format!("t{t}r{round}.sst"), &entries);
                    db.ingest_external_files(&[path], ingest_opts())
                        .wait()
                        .unwrap();
                    for key in &keys {
                        assert_eq!(db.get(key).unwrap().as_deref(), Some(value.as_bytes()));
                    }
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
}
