//! Property tests: a walk through a `CacheOnly` handle, interrupted by a
//! `WouldBlock` at every block it lacks and resumed after each poll, yields
//! exactly what the same walk through a `Blocking` handle yields: the same
//! positions after every seek and step, the same pages, the same stream.
//!
//! The block cache is disabled or tiny, so nearly every block a walk needs
//! interrupts it, and the walks seek, step both ways, flip direction, stop
//! at bounds and prefixes, and page through a transaction's own writes.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use proptest::prelude::*;
use regolith::{
    Error, IoBudget, IoQueue, MergeOperator, OptimisticTransactionDb, Options, OwnedSnapshotIter,
    ReadMode, ScanCheck, ScanDirection, Transaction, TransactionError, TxnOptions,
};
use tempfile::TempDir;

/// More `WouldBlock`s than one operation here can meet.
const MOST_WAITS: usize = 100_000;

/// One key and its value.
type Entry = (Vec<u8>, Vec<u8>);

struct Concat;

impl MergeOperator for Concat {
    fn name(&self) -> &'static str {
        "concat"
    }

    fn full_merge(&self, _key: &[u8], base: Option<&[u8]>, operands: &[&[u8]]) -> Option<Vec<u8>> {
        let mut out = base.map(<[u8]>::to_vec).unwrap_or_default();
        for operand in operands {
            out.extend_from_slice(operand);
        }
        Some(out)
    }
}

#[derive(Clone, Debug)]
enum Write {
    Put(u8, u8),
    Delete(u8),
    Merge(u8, u8),
    Flush,
}

fn key(k: u8) -> Vec<u8> {
    // Two lengths, so one key can be a prefix of another.
    if k.is_multiple_of(4) {
        format!("k{:02}", k / 4).into_bytes()
    } else {
        format!("k{:02}-{}", k / 4, k % 4).into_bytes()
    }
}

fn write_strategy() -> impl Strategy<Value = Write> {
    prop_oneof![
        5 => (any::<u8>(), any::<u8>()).prop_map(|(k, v)| Write::Put(k % 120, v)),
        2 => any::<u8>().prop_map(|k| Write::Delete(k % 120)),
        2 => (any::<u8>(), any::<u8>()).prop_map(|(k, v)| Write::Merge(k % 120, v)),
        1 => Just(Write::Flush),
    ]
}

#[derive(Clone, Debug)]
enum Step {
    First,
    Last,
    Seek(u8),
    SeekBounded(u8, u8),
    SeekForPrev(u8),
    SeekPrefix(u8),
    Next(u8),
    Prev(u8),
}

fn step_strategy() -> impl Strategy<Value = Step> {
    prop_oneof![
        1 => Just(Step::First),
        1 => Just(Step::Last),
        2 => any::<u8>().prop_map(|k| Step::Seek(k % 130)),
        2 => (any::<u8>(), any::<u8>()).prop_map(|(a, b)| Step::SeekBounded(a % 130, b % 130)),
        2 => any::<u8>().prop_map(|k| Step::SeekForPrev(k % 130)),
        1 => any::<u8>().prop_map(|k| Step::SeekPrefix(k % 130)),
        4 => (1u8..12).prop_map(Step::Next),
        3 => (1u8..12).prop_map(Step::Prev),
    ]
}

fn open(dir: &TempDir, cache: usize) -> OptimisticTransactionDb {
    let opts = Options::default()
        .block_size(128)
        .block_cache_size(cache)
        .merge_operator(Some(Arc::new(Concat)));
    OptimisticTransactionDb::open(dir.path(), opts).unwrap()
}

fn apply(db: &OptimisticTransactionDb, writes: &[Write]) {
    let db = db.db();
    for write in writes {
        match write {
            Write::Put(k, v) => db.put(&key(*k), &[*v; 3]).unwrap(),
            Write::Delete(k) => db.delete(&key(*k)).unwrap(),
            Write::Merge(k, v) => db.merge(&key(*k), &[*v]).unwrap(),
            Write::Flush => db.flush().unwrap(),
        }
    }
    db.flush().unwrap();
}

/// Run one step on a cursor.
fn run(iter: &mut OwnedSnapshotIter, step: &Step) {
    match step {
        Step::First => iter.seek_to_first(),
        Step::Last => iter.seek_to_last(),
        Step::Seek(k) => iter.seek(&key(*k)),
        Step::SeekBounded(a, b) => iter.seek_bounded(&key(*a), &key(*b)),
        Step::SeekForPrev(k) => iter.seek_for_prev(&key(*k)),
        Step::SeekPrefix(k) => iter.seek_prefix(&key(*k)[..2]),
        Step::Next(_) => iter.next(),
        Step::Prev(_) => iter.prev(),
    }
}

/// Where a cursor stands: its entry, or why it has none.
fn position(iter: &OwnedSnapshotIter) -> Result<Option<Entry>, String> {
    if iter.valid() {
        return Ok(Some((
            iter.key().unwrap().to_vec(),
            iter.value().unwrap().to_vec(),
        )));
    }
    iter.status().map(|()| None).map_err(|e| e.to_string())
}

/// Finish whatever a `WouldBlock` interrupted: poll, resume, repeat.
fn settle(iter: &mut OwnedSnapshotIter, queue: &mut IoQueue) -> usize {
    let mut waits = 0;
    while matches!(iter.status(), Err(Error::WouldBlock(_))) {
        waits += 1;
        assert!(waits < MOST_WAITS, "the walk stopped making progress");
        queue.poll(IoBudget::ALL);
        iter.resume();
    }
    waits
}

/// Page through a transaction's view with a cursor, calling again after
/// every `WouldBlock`.
fn pages(
    txn: &Transaction,
    queue: &mut IoQueue,
    range: (u8, u8),
    direction: ScanDirection,
    page_bytes: usize,
) -> Vec<(Vec<u8>, Vec<u8>)> {
    let (lo, hi) = (key(range.0), key(range.1));
    let mut cursor = txn.cursor(Some(&lo), Some(&hi), direction, ScanCheck::Stretch);
    let mut got = Vec::new();
    let mut waits = 0;
    loop {
        match cursor.next_page(txn, page_bytes) {
            Ok(page) => {
                got.extend(page.entries.into_iter().map(|(k, v)| (k, v.to_vec())));
                if page.done {
                    return got;
                }
            }
            Err(TransactionError::WouldBlock(_)) => {
                waits += 1;
                assert!(waits < MOST_WAITS);
                queue.poll(IoBudget::ALL);
            }
            Err(other) => panic!("{other}"),
        }
        assert!(got.len() <= 1_000, "the walk repeats entries");
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 48,
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn a_resumed_cursor_stands_where_a_blocking_one_does(
        writes in prop::collection::vec(write_strategy(), 1..160),
        steps in prop::collection::vec(step_strategy(), 1..40),
        cache in prop_oneof![Just(0usize), Just(64 * 1024)],
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir, cache);
        apply(&db, &writes);
        let db = db.db();
        let mut queue = db.io_queue();
        // No write lands between the two, so both read one sequence.
        let mut blocking = db.snapshot().into_owned_iter();
        let mut cache_only = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()))
            .into_owned_iter();
        let (mut waits, mut entries) = (0, 0);
        for step in &steps {
            let repeat = match step {
                Step::Next(n) | Step::Prev(n) => *n,
                _ => 1,
            };
            for _ in 0..repeat {
                run(&mut blocking, step);
                run(&mut cache_only, step);
                waits += settle(&mut cache_only, &mut queue);
                let at = position(&blocking);
                entries += usize::from(matches!(at, Ok(Some(_))));
                prop_assert_eq!(position(&cache_only), at, "after {:?}", step);
            }
        }
        // With no cache every entry a walk reaches was read through a wait.
        prop_assert!(cache != 0 || entries == 0 || waits > 0);
    }

    #[test]
    fn a_resumed_stream_yields_the_blocking_stream(
        writes in prop::collection::vec(write_strategy(), 1..160),
        range in (any::<u8>(), any::<u8>()),
        cache in prop_oneof![Just(0usize), Just(64 * 1024)],
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir, cache);
        apply(&db, &writes);
        let db = db.db();
        let mut queue = db.io_queue();
        let (lo, hi) = (key(range.0 % 130), key(range.1 % 130));
        let blocking: Vec<_> = db
            .snapshot()
            .scan_stream(Some(&lo), Some(&hi))
            .map(|item| item.map(|(k, v)| (k, v.to_vec())).unwrap())
            .collect();
        let snapshot = db.snapshot().with_read_mode(ReadMode::CacheOnly(queue.id()));
        let mut stream = snapshot.scan_stream(Some(&lo), Some(&hi));
        let mut got = Vec::new();
        let mut waits = 0;
        loop {
            match stream.next() {
                None => break,
                Some(Ok((k, v))) => got.push((k, v.to_vec())),
                Some(Err(Error::WouldBlock(_))) => {
                    waits += 1;
                    prop_assert!(waits < MOST_WAITS);
                    queue.poll(IoBudget::ALL);
                }
                Some(Err(other)) => panic!("{other}"),
            }
        }
        prop_assert_eq!(got, blocking);
    }

    #[test]
    fn a_transaction_cursor_pages_as_a_blocking_one(
        writes in prop::collection::vec(write_strategy(), 1..160),
        own in prop::collection::vec(write_strategy(), 0..20),
        range in (any::<u8>(), any::<u8>()),
        reverse in any::<bool>(),
        page_bytes in prop_oneof![Just(0usize), Just(17), Just(1 << 20)],
        cache in prop_oneof![Just(0usize), Just(64 * 1024)],
    ) {
        let dir = TempDir::new().unwrap();
        let db = open(&dir, cache);
        apply(&db, &writes);
        let mut queue = db.db().io_queue();
        let direction = if reverse { ScanDirection::Reverse } else { ScanDirection::Forward };
        let range = (range.0 % 130, range.1 % 130);
        let own_writes = |txn: &Transaction| {
            for write in &own {
                match write {
                    Write::Put(k, v) => txn.put(&key(*k), &[*v; 2]).unwrap(),
                    Write::Delete(k) => txn.delete(&key(*k)).unwrap(),
                    Write::Merge(k, v) => txn.merge(&key(*k), &[*v]).unwrap(),
                    Write::Flush => {}
                }
            }
        };
        let blocking = db.begin(&TxnOptions::new());
        own_writes(&blocking);
        let expected = pages(&blocking, &mut queue, range, direction, page_bytes);
        let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
        own_writes(&txn);
        let got = pages(&txn, &mut queue, range, direction, page_bytes);
        prop_assert_eq!(got, expected);
    }
}
