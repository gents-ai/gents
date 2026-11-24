//! The streaming surfaces: iterating a cursor as a Rust iterator, and
//! writing a stream whose length the caller does not control.
//!
//! Both exist so a consumer can hold a page rather than a whole data set.
//! The read side is what a `Stream` gets built on outside this crate; the
//! write side bounds its own memory and says so in what it gives up.

// Native-only. wasm-pack builds every test target for wasm32, and these
// use the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use regolith::{Db, Options, StreamOptions, TxnOptions};
use tempfile::TempDir;

fn db_with(entries: &[(&str, &str)]) -> (Db, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    for (key, value) in entries {
        db.put(key.as_bytes(), value.as_bytes()).expect("put");
    }
    (db, dir)
}

#[test]
fn a_snapshot_cursor_iterates_in_key_order() {
    let (db, _dir) = db_with(&[("b", "2"), ("a", "1"), ("c", "3")]);

    let collected: Vec<(Vec<u8>, Vec<u8>)> = db
        .snapshot()
        .owned_iter()
        .into_iter()
        .map(|item| {
            let (key, value) = item.unwrap();
            (key, value.to_vec())
        })
        .collect();

    assert_eq!(
        collected,
        vec![
            (b"a".to_vec(), b"1".to_vec()),
            (b"b".to_vec(), b"2".to_vec()),
            (b"c".to_vec(), b"3".to_vec()),
        ]
    );
}

/// The value side is a `DbSlice`, so iteration hands over the bytes the
/// database already holds rather than copying each one out.
#[test]
fn iteration_yields_values_without_copying_them() {
    let (db, _dir) = db_with(&[("k", "value-bytes")]);

    let (key, value) = db
        .snapshot()
        .owned_iter()
        .into_iter()
        .next()
        .expect("one entry")
        .unwrap();

    assert_eq!(key, b"k".to_vec());
    // `DbSlice` derefs to the stored bytes; no `to_vec` needed to read it.
    assert_eq!(&*value, b"value-bytes");
}

#[test]
fn a_cursor_iterates_backward_on_request() {
    let (db, _dir) = db_with(&[("a", "1"), ("b", "2"), ("c", "3")]);

    let keys: Vec<Vec<u8>> = db
        .snapshot()
        .owned_iter()
        .entries_rev()
        .map(|item| item.unwrap().0)
        .collect();

    assert_eq!(keys, vec![b"c".to_vec(), b"b".to_vec(), b"a".to_vec()]);
}

/// A cursor the caller positioned keeps that position: seeking and then
/// iterating resumes from the seek instead of restarting the range.
#[test]
fn iteration_resumes_from_a_seek() {
    let (db, _dir) = db_with(&[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]);

    let snapshot = db.snapshot();
    let mut cursor = snapshot.owned_iter();
    cursor.seek(b"c");

    let keys: Vec<Vec<u8>> = cursor.entries().map(|item| item.unwrap().0).collect();

    assert_eq!(keys, vec![b"c".to_vec(), b"d".to_vec()]);
}

/// Laziness is the point: a cursor must not read the whole range to serve
/// a few entries, which is what `take` relies on.
#[test]
fn iteration_stops_early_without_draining_the_range() {
    let entries: Vec<(String, String)> = (0..10_000)
        .map(|i| (format!("key/{i:06}"), format!("value/{i}")))
        .collect();
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    for (key, value) in &entries {
        db.put(key.as_bytes(), value.as_bytes()).expect("put");
    }

    let first_three: Vec<Vec<u8>> = db
        .snapshot()
        .owned_iter()
        .into_iter()
        .take(3)
        .map(|item| item.unwrap().0)
        .collect();

    assert_eq!(
        first_three,
        vec![
            b"key/000000".to_vec(),
            b"key/000001".to_vec(),
            b"key/000002".to_vec(),
        ]
    );
}

#[test]
fn an_empty_database_iterates_to_nothing() {
    let (db, _dir) = db_with(&[]);
    assert_eq!(db.snapshot().owned_iter().into_iter().count(), 0);
    assert_eq!(db.snapshot().owned_iter().entries_rev().count(), 0);
}

#[test]
fn a_streaming_writer_applies_every_write() {
    const ENTRIES: usize = 2_000;

    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");

    let mut writer = db.streaming_writer(StreamOptions {
        max_buffered_bytes: 4 * 1024,
        ..StreamOptions::default()
    });
    for i in 0..ENTRIES {
        writer
            .put(format!("key/{i:06}").as_bytes(), format!("v{i}").as_bytes())
            .expect("put");
    }
    let sequence = writer.finish().expect("finish");

    assert!(sequence > 0, "a stream that wrote must report its sequence");
    for i in 0..ENTRIES {
        assert_eq!(
            db.get(format!("key/{i:06}").as_bytes())
                .expect("get")
                .as_deref(),
            Some(format!("v{i}").as_bytes()),
            "entry {i} did not survive the stream"
        );
    }
}

/// The budget is the whole point: buffered bytes must fall back to zero
/// as the stream runs, rather than growing with the input.
#[test]
fn a_streaming_writer_bounds_what_it_buffers() {
    const BUDGET: usize = 4 * 1024;
    const VALUE_LEN: usize = 256;

    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");

    let mut writer = db.streaming_writer(StreamOptions {
        max_buffered_bytes: BUDGET,
        ..StreamOptions::default()
    });

    let value = vec![b'x'; VALUE_LEN];
    let mut peak = 0;
    for i in 0..2_000 {
        writer
            .put(format!("key/{i:06}").as_bytes(), &value)
            .expect("put");
        peak = peak.max(writer.buffered_bytes());
    }

    assert!(
        peak < BUDGET + VALUE_LEN + 64,
        "buffered {peak} bytes against a {BUDGET} byte budget: the writer is \
         accumulating the stream instead of flushing it"
    );
    writer.finish().expect("finish");
}

#[test]
fn a_streaming_writer_deletes_through_the_stream() {
    let (db, _dir) = db_with(&[("keep", "1"), ("drop", "2")]);

    let mut writer = db.streaming_writer(StreamOptions::default());
    writer.delete(b"drop").expect("delete");
    writer.finish().expect("finish");

    assert_eq!(db.get(b"keep").expect("get").as_deref(), Some(&b"1"[..]));
    assert_eq!(db.get(b"drop").expect("get"), None);
}

#[test]
fn a_streaming_writer_that_wrote_nothing_finishes_cleanly() {
    let (db, _dir) = db_with(&[]);
    let writer = db.streaming_writer(StreamOptions::default());
    assert_eq!(writer.finish().expect("finish"), 0);
}

/// Everything flushed before a drop is already durable, so it stays.
/// Only what was still buffered goes.
#[test]
fn dropping_a_writer_keeps_what_it_already_flushed() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");

    let value = vec![b'x'; 512];
    {
        let mut writer = db.streaming_writer(StreamOptions {
            max_buffered_bytes: 1024,
            ..StreamOptions::default()
        });
        for i in 0..64 {
            writer
                .put(format!("key/{i:04}").as_bytes(), &value)
                .expect("put");
        }
        // Dropped without `finish`.
    }

    // The budget is 1 KiB against 64 entries of 512 bytes, so most of the
    // stream flushed long before the drop.
    let survived = (0..64)
        .filter(|i| {
            db.get(format!("key/{i:04}").as_bytes())
                .expect("get")
                .is_some()
        })
        .count();
    assert!(
        survived >= 32,
        "only {survived} of 64 entries survived, so flushed writes are being lost"
    );
}

#[test]
fn a_streaming_write_is_visible_to_a_later_snapshot() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");

    let before = db.snapshot();
    let mut writer = db.streaming_writer(StreamOptions::default());
    writer.put(b"k", b"v").expect("put");
    let sequence = writer.finish().expect("finish");

    assert_eq!(before.get(b"k").expect("get"), None);
    assert!(db.snapshot().sequence() >= sequence);
    assert_eq!(
        db.snapshot().get(b"k").expect("get").as_deref(),
        Some(&b"v"[..])
    );
}

#[test]
fn scan_stream_respects_the_range_bounds() {
    let (db, _dir) = db_with(&[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4"), ("e", "5")]);

    let keys: Vec<Vec<u8>> = db
        .scan_stream(Some(b"b"), Some(b"d"))
        .expect("scan_stream")
        .map(|entry| entry.expect("scan row").0)
        .collect();

    assert_eq!(keys, vec![b"b".to_vec(), b"c".to_vec()]);
}

#[test]
fn scan_stream_with_open_bounds_covers_everything() {
    let (db, _dir) = db_with(&[("a", "1"), ("b", "2"), ("c", "3")]);

    let keys: Vec<Vec<u8>> = db
        .scan_stream(None, None)
        .expect("scan_stream")
        .map(|entry| entry.expect("scan row").0)
        .collect();

    assert_eq!(keys, vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec()]);
}

#[test]
fn scan_stream_agrees_with_the_materializing_scan() {
    let entries: Vec<(String, String)> = (0..500)
        .map(|i| (format!("key/{i:04}"), format!("value/{i}")))
        .collect();
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    for (key, value) in &entries {
        db.put(key.as_bytes(), value.as_bytes()).expect("put");
    }

    let materialized = db.scan(Some(b"key/0100"), Some(b"key/0200")).expect("scan");
    let streamed: Vec<(Vec<u8>, Vec<u8>)> = db
        .scan_stream(Some(b"key/0100"), Some(b"key/0200"))
        .expect("scan_stream")
        .map(|entry| {
            let (key, value) = entry.expect("scan row");
            (key, value.to_vec())
        })
        .collect();

    assert_eq!(streamed, materialized);
}

mod txn_scan {
    use super::*;
    use regolith::{IsolationLevel, OptimisticTransactionDb};
    use std::sync::Arc;

    fn txn_db(entries: &[(&str, &str)]) -> (Arc<OptimisticTransactionDb>, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let db =
            Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open"));
        for (key, value) in entries {
            db.db().put(key.as_bytes(), value.as_bytes()).expect("put");
        }
        (db, dir)
    }

    fn collect(
        txn: &regolith::Transaction,
        lo: Option<&[u8]>,
        hi: Option<&[u8]>,
    ) -> Vec<(String, String)> {
        txn.scan_stream(lo, hi)
            .map(|item| {
                let (k, v) = item.unwrap();
                (
                    String::from_utf8_lossy(&k).into_owned(),
                    String::from_utf8_lossy(&v).into_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn a_transaction_scan_sees_the_committed_state() {
        let (db, _dir) = txn_db(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

        assert_eq!(
            collect(&txn, None, None),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into())
            ]
        );
    }

    #[test]
    fn a_transaction_scan_sees_its_own_uncommitted_writes() {
        let (db, _dir) = txn_db(&[("a", "1"), ("c", "3")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        txn.put(b"b", b"2").expect("put");

        assert_eq!(
            collect(&txn, None, None),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "2".into()),
                ("c".into(), "3".into())
            ]
        );
    }

    #[test]
    fn a_buffered_write_overrides_the_committed_value() {
        let (db, _dir) = txn_db(&[("a", "1"), ("b", "old"), ("c", "3")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        txn.put(b"b", b"new").expect("put");

        assert_eq!(
            collect(&txn, None, None),
            vec![
                ("a".into(), "1".into()),
                ("b".into(), "new".into()),
                ("c".into(), "3".into())
            ]
        );
    }

    #[test]
    fn a_buffered_delete_hides_the_committed_value() {
        let (db, _dir) = txn_db(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        txn.delete(b"b").expect("delete");

        assert_eq!(
            collect(&txn, None, None),
            vec![("a".into(), "1".into()), ("c".into(), "3".into())]
        );
    }

    #[test]
    fn a_transaction_scan_respects_the_range_bounds() {
        let (db, _dir) = txn_db(&[("a", "1"), ("b", "2"), ("c", "3"), ("d", "4")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        txn.put(b"bb", b"buffered").expect("put");

        assert_eq!(
            collect(&txn, Some(b"b"), Some(b"d")),
            vec![
                ("b".into(), "2".into()),
                ("bb".into(), "buffered".into()),
                ("c".into(), "3".into())
            ]
        );
    }

    #[test]
    fn a_transaction_scan_over_an_empty_range_yields_nothing() {
        let (db, _dir) = txn_db(&[("a", "1")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));
        assert_eq!(collect(&txn, Some(b"m"), Some(b"n")), vec![]);
    }

    #[test]
    fn a_transaction_read_can_avoid_copying_the_value() {
        let (db, _dir) = txn_db(&[("k", "committed-bytes")]);
        let txn = db.begin(&TxnOptions::new().isolation(IsolationLevel::Serializable));

        let slice = txn.get_slice(b"k").expect("get_slice").expect("present");
        assert_eq!(&*slice, b"committed-bytes");

        txn.put(b"k", b"buffered-bytes").expect("put");
        let slice = txn.get_slice(b"k").expect("get_slice").expect("present");
        assert_eq!(&*slice, b"buffered-bytes");
    }
}
