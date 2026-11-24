//! A scan that fails mid-range must not read as one that finished.
//!
//! A scan that dies on a corrupt block, or on a key the merge operator
//! declines, must not end like one that reached the end of its range, or a
//! caller that only iterates sees a short answer and no reason for it. Every
//! stream carries the failure as an item instead: the rows are a prefix, one
//! `Err` follows, and then the stream is finished. Nothing has to be asked
//! afterwards.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// the filesystem, which does not exist there.
#![cfg(not(target_arch = "wasm32"))]

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use regolith::{Db, DbSlice, Error, MergeOperator, Options, TxnOptions, WriteBatch};

fn small_options() -> Options {
    Options::default()
        .write_buffer_size(64 * 1024)
        .block_size(4 * 1024)
        .block_cache_size(0)
        .target_file_size(256 * 1024)
        .l0_compaction_trigger(8)
        .max_background_compactions(0)
        .inline_compaction(true)
}

const KEYS: u64 = 20_000;

fn build(dir: &std::path::Path) {
    let db = Db::open(dir, small_options()).unwrap();
    let value = [b'v'; 128];
    let mut batch = WriteBatch::new();
    for i in 0..KEYS {
        batch.put(&i.to_be_bytes(), &value);
        if batch.buffered_bytes() >= 64 * 1024 {
            let full = std::mem::take(&mut batch);
            regolith::through_stalls(|| db.write(full.clone())).unwrap();
        }
    }
    regolith::through_stalls(|| db.write(batch.clone())).unwrap();
    db.flush().unwrap();
    db.close().unwrap();
}

/// Damage the middle of the largest SSTable, past its first data block, so a
/// scan gets going and then hits the damage rather than failing at open.
fn damage_a_data_block(dir: &std::path::Path) {
    let mut biggest: Option<(u64, std::path::PathBuf)> = None;
    for entry in walk(dir) {
        if entry.extension().and_then(|e| e.to_str()) != Some("sst") {
            continue;
        }
        let len = std::fs::metadata(&entry).unwrap().len();
        if biggest.as_ref().is_none_or(|(best, _)| len > *best) {
            biggest = Some((len, entry));
        }
    }
    let (len, path) = biggest.expect("the load must have produced an SSTable");
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();

    // A whole block's worth of garbage in the middle of the data area. The
    // checksum catches it, which is the point: the read fails rather than
    // returning wrong bytes.
    let offset = len / 3;
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut original = vec![0u8; 8 * 1024];
    file.read_exact(&mut original).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    let flipped: Vec<u8> = original.iter().map(|b| !b).collect();
    file.write_all(&flipped).unwrap();
    file.sync_all().unwrap();
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

#[test]
fn a_scan_cut_short_by_damage_ends_with_an_err_instead_of_looking_complete() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path());
    damage_a_data_block(dir.path());

    let db = Db::open(dir.path(), small_options()).unwrap();
    let mut scan = db.scan_stream(None, None).unwrap();
    let mut rows = 0u64;
    let mut errors = 0u64;
    for item in scan.by_ref() {
        match item {
            Ok(_) => {
                assert_eq!(errors, 0, "nothing follows the error");
                rows += 1;
            }
            Err(_) => errors += 1,
        }
    }
    assert!(
        scan.next().is_none(),
        "the stream is finished after the error"
    );

    // Either outcome is acceptable on its own; what is not acceptable is a
    // short scan with no error. If the damage happened to land somewhere a
    // scan never reads, the scan is complete and has no `Err`.
    assert!(errors <= 1, "the error is handed out once");
    assert_eq!(
        errors == 1,
        rows < KEYS,
        "a short scan must end with its reason, and a full one must not carry one"
    );
}

/// The iterator form of a cursor carries the failure as an item, so no caller
/// has to remember to ask: a short answer ends with its reason.
#[test]
fn the_entries_of_a_cursor_end_with_an_err_when_damage_cut_them_short() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path());
    damage_a_data_block(dir.path());

    let db = Db::open(dir.path(), small_options()).unwrap();
    let mut entries = db.snapshot().into_owned_iter().entries();
    let mut rows = 0u64;
    let mut failed = false;
    for item in entries.by_ref() {
        match item {
            Ok(_) => assert!(!failed, "nothing follows the error"),
            Err(_) => failed = true,
        }
        rows += u64::from(!failed);
    }
    assert!(
        entries.next().is_none(),
        "the iterator is finished after the error"
    );
    assert_eq!(
        failed,
        rows < KEYS,
        "a short scan ended with its reason, a full one without"
    );
}

/// The same through a transaction's cursor, a page at a time.
#[test]
fn a_transaction_cursor_cut_short_by_damage_returns_the_error() {
    use regolith::{OptimisticTransactionDb, ScanCheck, ScanDirection};

    let dir = tempfile::tempdir().unwrap();
    build(dir.path());
    damage_a_data_block(dir.path());

    let db = OptimisticTransactionDb::open(dir.path(), small_options()).unwrap();
    let txn = db.begin(&TxnOptions::new());
    let mut cursor = txn.cursor(None, None, ScanDirection::Forward, ScanCheck::Stretch);
    let mut rows = 0u64;
    let outcome = loop {
        match cursor.next_page(&txn, 64 * 1024) {
            Ok(page) => {
                rows += page.entries.len() as u64;
                if page.done {
                    break Ok(());
                }
            }
            Err(e) => break Err(e),
        }
    };
    match outcome {
        Err(_) => {
            assert!(rows < KEYS);
            assert!(cursor.next_page(&txn, 1).is_err(), "the error is final");
        }
        Ok(()) => assert_eq!(rows, KEYS, "a scan that ended must have returned every row"),
    }
}

#[test]
fn an_undamaged_scan_reports_success_and_returns_everything() {
    let dir = tempfile::tempdir().unwrap();
    build(dir.path());

    let db = Db::open(dir.path(), small_options()).unwrap();
    let rows = db
        .scan_stream(None, None)
        .unwrap()
        .collect::<regolith::Result<Vec<_>>>()
        .expect("an intact store must scan clean");
    assert_eq!(rows.len() as u64, KEYS);
}

/// The same contract on the transaction side, stronger: the items carry the
/// error, so a merged stream that loses its snapshot cursor cannot finish on
/// the buffered writes alone and look like a range that ended.
#[test]
fn a_transaction_scan_cut_short_says_so_too() {
    use regolith::TransactionDb;

    let dir = tempfile::tempdir().unwrap();
    let tdb = TransactionDb::open(dir.path(), small_options()).unwrap();
    for i in 0..10u64 {
        regolith::through_stalls(|| tdb.db().put(format!("k{i:02}").as_bytes(), b"v")).unwrap();
    }

    let txn = tdb.begin(&TxnOptions::new());
    txn.put(b"zzz", b"buffered").unwrap();

    let rows: Vec<_> = txn.scan_stream(None, None).collect();
    assert_eq!(rows.len(), 11, "ten committed rows plus the buffered write");
    assert!(rows.iter().all(Result::is_ok));

    // Closing the database makes every iterator built afterwards carry a
    // terminal error. The stream must hand it out as an item, not return the
    // one buffered write and look like a complete range.
    tdb.db().close().unwrap();

    let after: Vec<_> = txn.scan_stream(None, None).collect();
    assert!(
        after.last().is_some_and(Result::is_err),
        "the snapshot side died, so the scan must end with the reason rather than \
         read as a complete range of {} rows",
        after.len()
    );
    assert_eq!(after.iter().filter(|item| item.is_err()).count(), 1);
}

/// Declines any operand `bad`, which a read of that key reports as an error.
struct Declines;

impl MergeOperator for Declines {
    fn name(&self) -> &'static str {
        "declines"
    }

    fn full_merge(&self, _: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        if operands.contains(&&b"bad"[..]) {
            return None;
        }
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        operands
            .iter()
            .for_each(|operand| out.extend_from_slice(operand));
        Some(out)
    }
}

type Item = regolith::Result<(Vec<u8>, DbSlice)>;

fn declining_options() -> Options {
    Options::default().merge_operator(Some(Arc::new(Declines)))
}

/// `k0 k1 [k2: a merge the operator declines] k3 k4`, flushed to an SSTable
/// when `flush` is set so the failure is met in a table and not a memtable.
fn declining(dir: &std::path::Path, flush: bool) -> Db {
    let db = Db::open(dir, declining_options()).unwrap();
    for key in ["k0", "k1", "k3", "k4"] {
        regolith::through_stalls(|| db.put(key.as_bytes(), b"v")).unwrap();
    }
    regolith::through_stalls(|| db.merge(b"k2", b"bad")).unwrap();
    if flush {
        db.flush().unwrap();
    }
    db
}

fn keys(items: &[Item]) -> Vec<String> {
    items
        .iter()
        .flatten()
        .map(|(key, _)| String::from_utf8(key.clone()).unwrap())
        .collect()
}

/// The rows before the failure, then one `Err`, then nothing, however often
/// the stream is asked.
fn assert_prefix_then_err_then_none(mut scan: impl Iterator<Item = Item>) {
    let items: Vec<Item> = scan.by_ref().collect();
    assert_eq!(items.len(), 3, "two rows and the error, nothing after it");
    assert_eq!(keys(&items), ["k0", "k1"]);
    assert!(
        matches!(items[2], Err(Error::Corruption(_))),
        "the last item is the failure"
    );
    for _ in 0..3 {
        assert!(scan.next().is_none(), "the stream stays finished");
    }
}

#[test]
fn a_db_scan_stream_yields_the_error_where_a_key_failed_then_ends() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = declining(dir.path(), flush);
        assert_prefix_then_err_then_none(db.scan_stream(None, None).unwrap());
    }
}

#[test]
fn a_snapshot_scan_stream_yields_the_error_where_a_key_failed_then_ends() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = declining(dir.path(), flush);
        let snapshot = db.snapshot();
        assert_prefix_then_err_then_none(snapshot.scan_stream(None, None));
        assert_prefix_then_err_then_none(snapshot.into_scan_stream(None, None));
    }
}

#[test]
fn an_error_on_the_first_key_is_the_first_item() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(dir.path(), declining_options()).unwrap();
    regolith::through_stalls(|| db.merge(b"a", b"bad")).unwrap();
    regolith::through_stalls(|| db.put(b"b", b"v")).unwrap();

    let mut scan = db.scan_stream(None, None).unwrap();
    assert!(matches!(scan.next(), Some(Err(Error::Corruption(_)))));
    assert!(scan.next().is_none());
}

/// A key the stream is not asked to reach cannot fail it: a bound that ends
/// the scan short of the declined key, and a start past it, both finish clean.
#[test]
fn a_range_that_does_not_reach_the_failure_ends_cleanly() {
    for flush in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let db = declining(dir.path(), flush);

        let before: Vec<Item> = db.scan_stream(None, Some(b"k2")).unwrap().collect();
        assert_eq!(keys(&before), ["k0", "k1"]);
        assert!(
            before.iter().all(Result::is_ok),
            "the range ended, it did not fail"
        );

        let after: Vec<Item> = db.scan_stream(Some(b"k3"), None).unwrap().collect();
        assert_eq!(keys(&after), ["k3", "k4"]);
        assert!(after.iter().all(Result::is_ok));
    }
}

/// The idiom the item type is for: one `collect` into a `Result`, and a
/// partial range cannot be taken for a whole one.
#[test]
fn collecting_into_a_result_surfaces_the_failure() {
    let dir = tempfile::tempdir().unwrap();
    let db = declining(dir.path(), false);
    let collected = db
        .scan_stream(None, None)
        .unwrap()
        .collect::<regolith::Result<Vec<_>>>();
    assert!(matches!(collected, Err(Error::Corruption(_))));
}
