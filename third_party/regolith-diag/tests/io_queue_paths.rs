//! Every read path through a `CacheOnly` handle (plan 4.10, D53).
//!
//! On a cold block cache each one returns `WouldBlock` without touching the
//! device: no SSTable read and no SSTable open, counted at the `Env`. Once
//! its owner polls the queue the wait names, the read run again returns
//! exactly what a `Blocking` read returns. Each layout below moves a
//! different read onto the device: data blocks, cached indexes and filters,
//! partitioned index leaves, files reopened under `max_open_files`, and a
//! disabled block cache, where only what landed for the queue carries the
//! read. Each layout runs plain and encrypted at rest.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::sync::Arc;

use common::device_env::{Device, DeviceEnv};
use common::keys::Keys;
use common::parted::{self, Parted};
use regolith::{
    Db, Error, IoBudget, IoQueue, IsolationLevel, OptimisticTransactionDb, Options, ReadMode,
    ScanCheck, ScanDirection, Snapshot, TransactionDb, TransactionError, TxnOptions, WouldBlock,
};
use tempfile::TempDir;

/// Keys written, `k000`..`k399`. Opening a database reads the first block of
/// every table (the column-family registry sorts first), so the reads below
/// stay among the keys in the middle, which no open has cached.
const KEYS: usize = 400;
/// The key the fill deletes, and merges nothing into after.
const DELETED: usize = 157;
/// More `WouldBlock`s than any one read here can meet: a read past it never
/// finished, which fails the test rather than spinning.
const MOST_WAITS: usize = 10_000;

/// The entries a walk yielded, keys without the column-family prefix.
type Walked = Vec<(Vec<u8>, Vec<u8>)>;

#[derive(Clone, Copy, Debug)]
enum Shape {
    /// Pinned indexes and filters: only data blocks are read.
    Pinned,
    /// Flat indexes and filters through the block cache.
    CachedMeta,
    /// Partitioned indexes, leaves through the block cache.
    Partitioned,
    /// One descriptor kept open, so reads reopen files.
    FewFiles,
    /// No block cache: a read finds its block only where it landed.
    NoCache,
}

/// A table layout, plain or encrypted at rest. On a sealed table the read a
/// queue runs opens each block (decrypts and verifies it) before it lands in
/// the block cache or for the queue, so every path below must answer on an
/// encrypted database exactly as it does on a plain one.
#[derive(Clone, Copy, Debug)]
struct Layout {
    shape: Shape,
    sealed: bool,
}

impl Layout {
    const fn plain(shape: Shape) -> Self {
        Self {
            shape,
            sealed: false,
        }
    }

    const fn sealed(shape: Shape) -> Self {
        Self {
            shape,
            sealed: true,
        }
    }
}

const LAYOUTS: [Layout; 10] = [
    Layout::plain(Shape::Pinned),
    Layout::plain(Shape::CachedMeta),
    Layout::plain(Shape::Partitioned),
    Layout::plain(Shape::FewFiles),
    Layout::plain(Shape::NoCache),
    Layout::sealed(Shape::Pinned),
    Layout::sealed(Shape::CachedMeta),
    Layout::sealed(Shape::Partitioned),
    Layout::sealed(Shape::FewFiles),
    Layout::sealed(Shape::NoCache),
];

/// `base` laid out as `layout` says.
fn laid_out(base: Options, layout: Layout) -> Options {
    let base = match layout.shape {
        Shape::Pinned => base,
        Shape::CachedMeta => base.cache_index_and_filter_blocks(true),
        Shape::Partitioned => base
            .partitioned_index(true)
            .cache_index_and_filter_blocks(true),
        Shape::FewFiles => base.max_open_files(1),
        Shape::NoCache => base.block_cache_size(0),
    };
    if layout.sealed {
        base.key_provider(Keys::new(&[1]))
    } else {
        base
    }
}

fn options(layout: Layout, env: Arc<DeviceEnv>) -> Options {
    laid_out(
        Options::default()
            .env(env)
            .block_size(256)
            .metadata_block_size(256)
            .merge_operator(Some(Arc::new(Parted))),
        layout,
    )
}

/// [`options`] with no merge operator.
fn options_without_merges(layout: Layout, env: Arc<DeviceEnv>) -> Options {
    laid_out(
        Options::default()
            .env(env)
            .block_size(256)
            .metadata_block_size(256),
        layout,
    )
}

fn key(i: usize) -> Vec<u8> {
    format!("k{i:03}").into_bytes()
}

/// Write every key into tables: several tables, merge operands in newer
/// tables over bases in older ones, and a deleted key.
fn fill(db: &Db) {
    for i in 0..KEYS {
        db.put(&key(i), &parted::value([i as u64, 0, 0])).unwrap();
    }
    db.flush().unwrap();
    for i in (0..KEYS).step_by(3) {
        db.merge(&key(i), &parted::add(1, 7)).unwrap();
    }
    db.delete(&key(DELETED)).unwrap();
    db.flush().unwrap();
    for i in (0..KEYS).step_by(5) {
        db.merge(&key(i), &parted::add(2, 1)).unwrap();
    }
    db.flush().unwrap();
}

/// A filled database reopened so the block cache is cold.
fn cold(layout: Layout) -> (TempDir, OptimisticTransactionDb, Arc<Device>) {
    let dir = TempDir::new().unwrap();
    let (env, device) = DeviceEnv::new();
    {
        let db =
            OptimisticTransactionDb::open(dir.path(), options(layout, Arc::clone(&env))).unwrap();
        fill(db.db());
        db.db().close().unwrap();
    }
    let db = OptimisticTransactionDb::open(dir.path(), options(layout, env)).unwrap();
    (dir, db, device)
}

/// Run `read` until it stops returning `WouldBlock`, polling `queue` after
/// each wait. Asserts the read never touched the device itself, and returns
/// its answer and how many times it waited.
fn drive<T>(
    device: &Device,
    queue: &mut IoQueue,
    mut read: impl FnMut() -> Result<T, Error>,
) -> (T, usize) {
    let mut waits = 0;
    loop {
        let before = device.touches();
        let outcome = read();
        assert_eq!(
            device.touches(),
            before,
            "a CacheOnly read touched the device itself"
        );
        match outcome {
            Ok(answer) => return (answer, waits),
            Err(Error::WouldBlock(WouldBlock::Io(wait))) => {
                waits += 1;
                assert!(waits < MOST_WAITS, "the read never finished");
                assert_eq!(wait.queue(), queue.id(), "a miss went to another queue");
                assert!(!wait.is_ready());
                queue.poll(IoBudget::ALL);
                assert!(wait.is_ready(), "the owner's poll completes its wait");
            }
            Err(other) => panic!("the read failed: {other}"),
        }
    }
}

/// A transaction's error as a plain one, so [`drive`] serves both.
fn plain(err: TransactionError) -> Error {
    match err {
        TransactionError::WouldBlock(wait) => Error::WouldBlock(wait),
        TransactionError::Engine(err) => err,
        other => panic!("the read failed: {other}"),
    }
}

type SnapshotRead = fn(&Db, &Snapshot) -> Result<String, Error>;

/// Every point and range read a snapshot offers, each answer rendered for
/// comparison.
fn snapshot_reads() -> Vec<(&'static str, SnapshotRead)> {
    vec![
        ("get", |_, s| s.get(&key(109)).map(|v| format!("{v:?}"))),
        ("get of a merged key", |_, s| {
            s.get(&key(115)).map(|v| format!("{v:?}"))
        }),
        ("get of a deleted key", |_, s| {
            s.get(&key(DELETED)).map(|v| format!("{v:?}"))
        }),
        ("get_slice", |_, s| {
            s.get_slice(&key(140))
                .map(|v| format!("{:?}", v.map(|v| v.to_vec())))
        }),
        ("get_slice_cf", |db, s| {
            s.get_slice_cf(&db.default_cf(), &key(141))
                .map(|v| format!("{:?}", v.map(|v| v.to_vec())))
        }),
        ("has", |_, s| s.has(&key(177)).map(|v| format!("{v:?}"))),
        ("has_cf", |db, s| {
            s.has_cf(&db.default_cf(), &key(178))
                .map(|v| format!("{v:?}"))
        }),
        ("get_size", |_, s| {
            s.get_size(&key(180)).map(|v| format!("{v:?}"))
        }),
        ("get_size_cf", |db, s| {
            s.get_size_cf(&db.default_cf(), &key(182))
                .map(|v| format!("{v:?}"))
        }),
        ("multi_get", |_, s| {
            s.multi_get(&[&key(101), &key(160), &key(DELETED), &key(219)])
                .map(|v| format!("{v:?}"))
        }),
        ("multi_get_cf", |db, s| {
            s.multi_get_cf(&db.default_cf(), &[&key(102), &key(161)])
                .map(|v| format!("{v:?}"))
        }),
        ("scan", |_, s| {
            s.scan(Some(&key(110)), Some(&key(170)))
                .map(|v| format!("{v:?}"))
        }),
        ("scan_cf", |db, s| {
            s.scan_cf(&db.default_cf(), None, None)
                .map(|v| format!("{v:?}"))
        }),
        ("scan_page", |_, s| {
            s.scan_page(Some(&key(130)), None, 25)
                .map(|v| format!("{v:?}"))
        }),
        ("scan_page_cf", |db, s| {
            s.scan_page_cf(&db.default_cf(), None, Some(&key(190)), 40)
                .map(|v| format!("{v:?}"))
        }),
    ]
}

#[test]
fn every_snapshot_read_waits_on_a_cold_cache_then_answers_as_blocking() {
    for layout in LAYOUTS {
        for (name, read) in snapshot_reads() {
            let (_dir, db, device) = cold(layout);
            let db = db.db();
            let mut queue = db.io_queue();
            let snapshot = db
                .snapshot()
                .with_read_mode(ReadMode::CacheOnly(queue.id()));
            let (answer, waits) = drive(&device, &mut queue, || read(db, &snapshot));
            assert!(waits > 0, "{layout:?} {name}: a cold read must wait");
            assert_eq!(
                answer,
                read(db, &db.snapshot()).unwrap(),
                "{layout:?} {name}"
            );
        }
    }
}

/// A filled database with no merge operator, which a size guard refuses,
/// reopened cold.
fn cold_plain(layout: Layout) -> (TempDir, Db, Arc<Device>) {
    let dir = TempDir::new().unwrap();
    let (env, device) = DeviceEnv::new();
    {
        let db = Db::open(dir.path(), options_without_merges(layout, Arc::clone(&env))).unwrap();
        for i in 0..KEYS {
            db.put(&key(i), &parted::value([i as u64, 0, 0])).unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), options_without_merges(layout, env)).unwrap();
    (dir, db, device)
}

#[test]
fn a_size_guard_answers_through_the_queue_as_it_does_blocking() {
    for layout in LAYOUTS {
        let (_dir, db, device) = cold_plain(layout);
        let mut queue = db.io_queue();
        let snapshot = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()));
        let (fits, waits) = drive(&device, &mut queue, || {
            snapshot.get_size_with_limit(&key(133), 1 << 20)
        });
        assert!(waits > 0, "{layout:?}");
        assert_eq!(fits, db.snapshot().get_size(&key(133)).unwrap());

        // Too small for any block: the read the queue ran refused it, and the
        // read run again reports that refusal.
        let (_dir, db, device) = cold_plain(layout);
        let mut queue = db.io_queue();
        let snapshot = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()));
        let mut outcome = None;
        for _ in 0..MOST_WAITS {
            let before = device.touches();
            let read = snapshot.get_size_with_limit(&key(133), 8);
            assert_eq!(device.touches(), before);
            match read {
                Err(Error::WouldBlock(_)) => {
                    queue.poll(IoBudget::ALL);
                }
                other => {
                    outcome = Some(other);
                    break;
                }
            }
        }
        assert!(
            matches!(outcome, Some(Err(Error::DataBlockLimitExceeded { .. }))),
            "{layout:?}: {outcome:?}"
        );
        assert!(matches!(
            db.snapshot().get_size_with_limit(&key(133), 8),
            Err(Error::DataBlockLimitExceeded { .. })
        ));
    }
}

/// Walk a cursor to its end, resuming it after every `WouldBlock`.
fn walk_cursor(
    device: &Device,
    queue: &mut IoQueue,
    seek: impl FnOnce(&mut regolith::OwnedSnapshotIter),
    mut iter: regolith::OwnedSnapshotIter,
    reverse: bool,
) -> (Walked, usize) {
    let mut waits = 0;
    let before = device.touches();
    seek(&mut iter);
    assert_eq!(device.touches(), before);
    let mut out = Vec::new();
    loop {
        if iter.valid() {
            out.push((iter.key().unwrap().to_vec(), iter.value().unwrap().to_vec()));
            let before = device.touches();
            if reverse {
                iter.prev();
            } else {
                iter.next();
            }
            assert_eq!(device.touches(), before, "a step touched the device");
            continue;
        }
        match iter.status() {
            Ok(()) => return (out, waits),
            Err(Error::WouldBlock(_)) => {
                waits += 1;
                assert!(waits < MOST_WAITS);
                queue.poll(IoBudget::ALL);
                let before = device.touches();
                iter.resume();
                assert_eq!(device.touches(), before, "a resume touched the device");
            }
            Err(other) => panic!("{other}"),
        }
    }
}

#[test]
fn cursors_resume_where_a_wait_stopped_them() {
    for layout in LAYOUTS {
        for reverse in [false, true] {
            let (_dir, db, device) = cold(layout);
            let db = db.db();
            let mut queue = db.io_queue();
            let iter = db
                .snapshot()
                .with_read_mode(ReadMode::CacheOnly(queue.id()))
                .into_owned_iter();
            let seek = |it: &mut regolith::OwnedSnapshotIter| {
                if reverse {
                    it.seek_to_last()
                } else {
                    it.seek_to_first()
                }
            };
            let (walked, waits) = walk_cursor(&device, &mut queue, seek, iter, reverse);
            assert!(waits > 0, "{layout:?}: a cold walk must wait");
            let mut expected = db.scan(None, None).unwrap();
            if reverse {
                expected.reverse();
            }
            assert_eq!(walked, expected, "{layout:?} reverse={reverse}");
        }
    }
}

#[test]
fn a_backward_step_that_waited_keeps_the_forward_bound() {
    for layout in LAYOUTS {
        let (_dir, db, _device) = cold(layout);
        let db = db.db();
        let mut queue = db.io_queue();
        let walk = |iter: &mut regolith::OwnedSnapshotIter, queue: &mut IoQueue| {
            let mut seen = Vec::new();
            let mut settle = |iter: &mut regolith::OwnedSnapshotIter| {
                while matches!(iter.status(), Err(Error::WouldBlock(_))) {
                    queue.poll(IoBudget::ALL);
                    iter.resume();
                }
            };
            // A step back from where the seek landed reads blocks the seek
            // did not, so a cold walk waits on it.
            iter.seek_bounded(&key(180), &key(200));
            settle(iter);
            iter.prev();
            settle(iter);
            while iter.valid() {
                seen.push(iter.key().unwrap().to_vec());
                iter.next();
                settle(iter);
            }
            seen
        };
        let mut blocking = db.snapshot().into_owned_iter();
        let mut cache_only = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()))
            .into_owned_iter();
        let expected = walk(&mut blocking, &mut queue);
        assert_eq!(expected.last(), Some(&key(199)), "the bound ends the walk");
        assert_eq!(walk(&mut cache_only, &mut queue), expected, "{layout:?}");
    }
}

#[test]
fn a_bounded_cursor_keeps_its_bound_across_a_resume() {
    for layout in LAYOUTS {
        let (_dir, db, device) = cold(layout);
        let db = db.db();
        let mut queue = db.io_queue();
        let iter = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()))
            .into_owned_iter();
        let seek = |it: &mut regolith::OwnedSnapshotIter| it.seek_bounded(&key(120), &key(250));
        let (walked, waits) = walk_cursor(&device, &mut queue, seek, iter, false);
        assert!(waits > 0);
        assert_eq!(walked, db.scan(Some(&key(120)), Some(&key(250))).unwrap());
    }
}

#[test]
fn streams_carry_on_after_a_wait() {
    for layout in LAYOUTS {
        let (_dir, db, device) = cold(layout);
        let db = db.db();
        let mut queue = db.io_queue();
        let snapshot = db
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()));
        let mut stream = snapshot.scan_stream(Some(&key(107)), Some(&key(299)));
        let mut got = Vec::new();
        let mut waits = 0;
        loop {
            let before = device.touches();
            let item = stream.next();
            assert_eq!(device.touches(), before);
            match item {
                None => break,
                Some(Ok((k, v))) => got.push((k, v.to_vec())),
                Some(Err(Error::WouldBlock(_))) => {
                    waits += 1;
                    assert!(waits < MOST_WAITS);
                    queue.poll(IoBudget::ALL);
                }
                Some(Err(other)) => panic!("{other}"),
            }
        }
        assert!(waits > 0, "{layout:?}");
        assert_eq!(got, db.scan(Some(&key(107)), Some(&key(299))).unwrap());
    }
}

#[test]
fn an_entries_walk_carries_on_after_a_wait() {
    for layout in LAYOUTS {
        let (_dir, db, device) = cold(layout);
        let db2 = db.db();
        let mut queue = db2.io_queue();
        let mut entries = db2
            .snapshot()
            .with_read_mode(ReadMode::CacheOnly(queue.id()))
            .into_owned_iter()
            .entries_rev();
        let mut got = Vec::new();
        loop {
            let before = device.touches();
            let item = entries.next();
            assert_eq!(device.touches(), before);
            match item {
                None => break,
                Some(Ok((k, v))) => got.push((k, v.to_vec())),
                Some(Err(Error::WouldBlock(_))) => {
                    queue.poll(IoBudget::ALL);
                }
                Some(Err(other)) => panic!("{other}"),
            }
        }
        let mut expected = db2.scan(None, None).unwrap();
        expected.reverse();
        assert_eq!(got, expected, "{layout:?}");
    }
}

type TxnRead = fn(&regolith::Transaction) -> Result<String, TransactionError>;

fn transaction_reads() -> Vec<(&'static str, TxnRead)> {
    vec![
        ("get", |t| t.get(&key(112)).map(|v| format!("{v:?}"))),
        ("get_slice", |t| {
            t.get_slice(&key(145))
                .map(|v| format!("{:?}", v.map(|v| v.to_vec())))
        }),
        ("get_for_update", |t| {
            t.get_for_update(&key(130)).map(|v| format!("{v:?}"))
        }),
        ("get_parts", |t| {
            t.get_parts(&key(160), &[1])
                .map(|v| format!("{:?}", v.map(|v| v.to_vec())))
        }),
        ("get of a key the transaction merged into", |t| {
            t.get(&key(121)).map(|v| format!("{v:?}"))
        }),
    ]
}

#[test]
fn every_transaction_read_waits_then_answers_as_blocking() {
    for layout in LAYOUTS {
        for (name, read) in transaction_reads() {
            let (_dir, db, device) = cold(layout);
            let mut queue = db.db().io_queue();
            let opts = TxnOptions::new()
                .isolation(IsolationLevel::DefraLevel)
                .read_mode(ReadMode::CacheOnly(queue.id()));
            let txn = db.begin(&opts);
            assert_eq!(txn.read_mode(), ReadMode::CacheOnly(queue.id()));
            assert_eq!(txn.io_queue(), Some(queue.id()));
            txn.merge(&key(121), &parted::add(0, 100)).unwrap();
            let (answer, waits) = drive(&device, &mut queue, || read(&txn).map_err(plain));
            assert!(waits > 0, "{layout:?} {name}: a cold read must wait");
            let blocking = db.begin(&TxnOptions::new().isolation(IsolationLevel::DefraLevel));
            blocking.merge(&key(121), &parted::add(0, 100)).unwrap();
            assert_eq!(answer, read(&blocking).unwrap(), "{layout:?} {name}");
        }
    }
}

/// Page through `[start, end)` with a cursor, calling again after every
/// `WouldBlock`.
fn pages(
    device: &Device,
    queue: &mut IoQueue,
    txn: &regolith::Transaction,
    direction: ScanDirection,
    page_bytes: usize,
) -> (Walked, usize) {
    let cache_only = txn.read_mode() != ReadMode::Blocking;
    let before = device.touches();
    let mut cursor = txn.cursor(
        Some(&key(3)),
        Some(&key(311)),
        direction,
        ScanCheck::Stretch,
    );
    assert!(!cache_only || device.touches() == before);
    let mut got: Walked = Vec::new();
    let mut waits = 0;
    loop {
        assert!(
            got.len() <= KEYS,
            "{direction:?} {page_bytes}: the walk repeats entries: {:?}",
            got.iter()
                .take(40)
                .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
                .collect::<Vec<_>>()
        );
        let before = device.touches();
        let page = cursor.next_page(txn, page_bytes);
        assert!(
            !cache_only || device.touches() == before,
            "a CacheOnly page touched the device"
        );
        match page {
            Ok(page) => {
                got.extend(page.entries.into_iter().map(|(k, v)| (k, v.to_vec())));
                if page.done {
                    return (got, waits);
                }
            }
            Err(TransactionError::WouldBlock(_)) => {
                waits += 1;
                assert!(waits < MOST_WAITS);
                queue.poll(IoBudget::ALL);
            }
            Err(other) => panic!("{other}"),
        }
    }
}

#[test]
fn a_transaction_cursor_pages_through_waits_without_skipping_or_repeating() {
    for layout in LAYOUTS {
        for direction in [ScanDirection::Forward, ScanDirection::Reverse] {
            for page_bytes in [0, 100, 1 << 20] {
                let (_dir, db, device) = cold(layout);
                let mut queue = db.db().io_queue();
                let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
                // The transaction's own writes merge into the walk.
                txn.put(&key(150), b"mine").unwrap();
                txn.delete(&key(151)).unwrap();
                txn.merge(&key(154), &parted::add(0, 1)).unwrap();
                let (got, waits) = pages(&device, &mut queue, &txn, direction, page_bytes);
                assert!(waits > 0, "{layout:?}");
                let blocking = db.begin(&TxnOptions::new());
                blocking.put(&key(150), b"mine").unwrap();
                blocking.delete(&key(151)).unwrap();
                blocking.merge(&key(154), &parted::add(0, 1)).unwrap();
                let (expected, _) = pages(&device, &mut queue, &blocking, direction, page_bytes);
                assert_eq!(got, expected, "{layout:?} {direction:?} {page_bytes}");
            }
        }
    }
}

#[test]
fn a_transaction_stream_carries_on_after_a_wait() {
    for layout in LAYOUTS {
        let (_dir, db, device) = cold(layout);
        let mut queue = db.db().io_queue();
        let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
        txn.merge(&key(109), &parted::add(0, 3)).unwrap();
        let mut got = Vec::new();
        let mut waits = 0;
        let mut stream = txn.scan_stream(Some(&key(0)), None);
        loop {
            let before = device.touches();
            let item = stream.next();
            assert_eq!(device.touches(), before);
            match item {
                None => break,
                Some(Ok((k, v))) => got.push((k, v.to_vec())),
                Some(Err(TransactionError::WouldBlock(_))) => {
                    waits += 1;
                    assert!(waits < MOST_WAITS);
                    queue.poll(IoBudget::ALL);
                }
                Some(Err(other)) => panic!("{other}"),
            }
        }
        drop(stream);
        assert!(waits > 0);
        let blocking = db.begin(&TxnOptions::new());
        blocking.merge(&key(109), &parted::add(0, 3)).unwrap();
        let expected: Vec<_> = blocking
            .scan_stream(Some(&key(0)), None)
            .map(|item| item.map(|(k, v)| (k, v.to_vec())).unwrap())
            .collect();
        assert_eq!(got, expected, "{layout:?}");
    }
}

#[test]
fn a_pessimistic_cursor_waits_on_a_key_read_past_its_snapshot() {
    for layout in LAYOUTS {
        let dir = TempDir::new().unwrap();
        let (env, device) = DeviceEnv::new();
        {
            let db = TransactionDb::open(dir.path(), options(layout, Arc::clone(&env))).unwrap();
            fill(db.db());
            db.db().close().unwrap();
        }
        let db = TransactionDb::open(dir.path(), options(layout, env)).unwrap();
        let mut queue = db.db().io_queue();
        let txn = db.begin(&TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id())));
        // A commit after the snapshot, then a locked read that sees it: the
        // scan serves that key at the lock horizon, a read of its own.
        db.db().put(&key(140), b"newer").unwrap();
        let (locked, _) = drive(&device, &mut queue, || {
            txn.get_for_update(&key(140)).map_err(plain)
        });
        assert_eq!(locked, Some(b"newer".to_vec()));
        let (got, waits) = pages(&device, &mut queue, &txn, ScanDirection::Forward, 64);
        assert!(waits > 0, "{layout:?}");
        assert!(got.contains(&(key(140), b"newer".to_vec())));
        assert_eq!(got.len(), (3..311).filter(|i| *i != DELETED).count());
    }
}

#[test]
fn a_blocking_handle_reads_the_device_as_before() {
    let (_dir, db, device) = cold(Layout::plain(Shape::Pinned));
    let db = db.db();
    let before = device.reads();
    assert!(db.snapshot().get(&key(103)).unwrap().is_some());
    assert!(
        device.reads() > before,
        "a Blocking read reads the device itself"
    );
    let _queue = db.io_queue();
    let before = device.reads();
    assert!(db.snapshot().get(&key(170)).unwrap().is_some());
    assert!(
        device.reads() > before,
        "opening a queue leaves Blocking reads alone"
    );
}

#[test]
fn reads_inside_commit_read_the_device_and_prepare_can_wait() {
    let (_dir, db, device) = cold(Layout::plain(Shape::Pinned));
    let mut queue = db.db().io_queue();
    let opts = TxnOptions::new().read_mode(ReadMode::CacheOnly(queue.id()));

    // `prepare` runs the callback in the transaction's mode: it waits.
    let mut txn = db.begin(&opts);
    txn.before_commit(|txn| {
        let found = txn.get(&key(222))?;
        txn.put(b"seen", &[u8::from(found.is_some())])
    });
    let before = device.touches();
    assert!(matches!(
        txn.prepare(),
        Err(TransactionError::WouldBlock(_))
    ));
    assert_eq!(device.touches(), before);
    // A callback that waited runs again from the start at the next prepare.
    let (_, waits) = drive(&device, &mut queue, || txn.prepare().map_err(plain));
    assert!(waits > 0);
    txn.commit().unwrap();
    assert_eq!(db.db().get(b"seen").unwrap(), Some(vec![1]));

    // `commit` does its own I/O: the callback's read blocks.
    let mut txn = db.begin(&opts);
    txn.before_commit(|txn| {
        let found = txn.get(&key(333))?;
        txn.put(b"seen", &[2 + u8::from(found.is_some())])
    });
    let before = device.reads();
    txn.commit().unwrap();
    assert!(
        device.reads() > before,
        "the read inside commit read the device"
    );
    assert_eq!(db.db().get(b"seen").unwrap(), Some(vec![3]));
}

#[test]
fn a_read_naming_a_dropped_queue_fails_loud() {
    let (_dir, db, _device) = cold(Layout::plain(Shape::Pinned));
    let db = db.db();
    let queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    drop(queue);
    assert!(matches!(
        snapshot.get(&key(201)),
        Err(Error::InvalidArgument(_))
    ));
}

#[test]
fn a_read_naming_another_databases_queue_fails_loud() {
    let (_dir, db, _device) = cold(Layout::plain(Shape::Pinned));
    let (_other_dir, other, _) = cold(Layout::plain(Shape::Pinned));
    let foreign = other.db().io_queue();
    let snapshot = db
        .db()
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(foreign.id()));
    assert!(matches!(
        snapshot.get(&key(201)),
        Err(Error::InvalidArgument(_))
    ));
}
