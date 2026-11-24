//! Where the Phase 7 per-core packages meet the Phase 6 integration, tested
//! across them.
//!
//! - **The read view and the flushes off the commit path** (7b x #271). Every
//!   publication is a compare-and-swap from the view it was built from:
//!   rotations under the pipeline mutex, the worker's flushes, a writer's
//!   flush after its commit, the stall step, compactions and ingests, all at
//!   once. No write is lost and no reader goes back.
//! - **Snapshot slots, the read view and compaction** (7a x 7b x #271). A
//!   snapshot pinned on one thread and dropped on another keeps its version
//!   through flushes and stripe compaction, and `wait_for_snapshots` returns
//!   once the last moved snapshot drops.
//! - **The block cache and sealed blocks** (7b x #266). A cache full of
//!   blocks readers hold refuses an insert and counts it (D58), a sealed
//!   block a queue read lands is still read by the re-run through the
//!   queue's landing, and a close while such a read waits completes it.
//! - **The open-file table, encryption and ingest** (7c1 x #266 x #268). A
//!   hard descriptor bound below the table count reads every sealed table
//!   and an ingested table at its sequence, through blocking and queued
//!   reads.
//! - **Tables renamed aside, backups and the sweep** (7c1 x #266 x #268). A
//!   table a compaction removes while a reader holds it is renamed aside;
//!   a checkpoint, a backup and a restore copy only what the version names,
//!   the last reader unlinks it, no second open can sweep under a live
//!   reader, and an open sweeps one a crash left.
//!
//! - **The commit side** (#276 x Phase 7). Commits through `commit_nowait`
//!   on each writer's own queue, with the flushes and steps they owe run as
//!   jobs on that queue, publish the read view like every other publisher;
//!   a `transact_async` attempt that suspends on a miss and re-runs on a
//!   conflict holds one snapshot pin per attempt and releases each once; a
//!   checkpoint run as a job, on the worker or on the caller's queue, under a
//!   hard open-file bound copies only what the version names.
//!
//! The column-family fence on each group-commit path, the group left owing
//! its fsync included, is tested where the fence is, in
//! `engine::commit::families`.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use common::keys::Keys;
use regolith::{
    BackupEngine, Checkpoint, Db, DbSlice, DurabilityMode, Error, IngestOptions, IoBudget, IoQueue,
    OptimisticTransactionDb, Options, ReadMode, RetryPolicy, SstFileWriter, Statistics, Ticker,
    TransactionError, TxnOptions, ready, through_stalls,
};
use tempfile::TempDir;

fn numbered(i: u64) -> Vec<u8> {
    format!("key/{i:05}").into_bytes()
}

/// Run `read` until it stops waiting, polling `queue` after each wait.
fn through_queue<T>(
    queue: &mut IoQueue,
    mut read: impl FnMut() -> regolith::Result<T>,
) -> regolith::Result<T> {
    for _ in 0..10_000 {
        match read() {
            Err(Error::WouldBlock(_)) => {
                queue.poll(IoBudget::ALL);
            }
            other => return other,
        }
    }
    panic!("the read never finished");
}

/// Poll `done` with a deadline and bounded backoff.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut backoff = Duration::from_micros(100);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_millis(10));
    }
}

/// The files in the table directory of the database at `db` whose names
/// contain `marker`.
fn table_dir_files(db: &Path, marker: &str) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(db.join("sst"))
        .map(|dir| dir.map(|e| e.unwrap().path()).collect())
        .unwrap_or_default();
    found.retain(|p| p.file_name().unwrap().to_string_lossy().contains(marker));
    found.sort();
    found
}

/// Every file name under `root`, recursively.
fn all_names(root: &Path) -> Vec<String> {
    let mut names = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else {
                names.push(path.file_name().unwrap().to_string_lossy().into_owned());
            }
        }
    }
    names
}

// The read view and every flush path ----------------------------------------

/// Writers, an ingest thread and readers at once, on `options`. Each writer
/// owns its keys and writes each one's versions in order; the ingest thread
/// installs tables of keys nobody else writes. A reader never sees a key's
/// version go back, and at the end every key holds its last version.
fn publishers_racing_readers(options: Options, ingest_dir: &Path) {
    const WRITERS: u64 = 4;
    const KEYS: u64 = 40;
    const VERSIONS: u64 = 60;
    const INGESTS: u64 = 12;
    let dir = TempDir::new().unwrap();
    let db = Db::open(dir.path(), options).unwrap();
    let done = AtomicBool::new(false);
    let reads = AtomicU64::new(0);
    let key = |w: u64, k: u64| format!("w{w}/{k:03}").into_bytes();
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                let db = &db;
                scope.spawn(move || {
                    for v in 1..=VERSIONS {
                        for k in 0..KEYS {
                            let mut value = v.to_be_bytes().to_vec();
                            value.resize(512, b'f');
                            through_stalls(|| db.put(&key(w, k), &value)).unwrap();
                        }
                    }
                })
            })
            .collect();
        let ingest = scope.spawn(|| {
            for n in 0..INGESTS {
                let source = ingest_dir.join(format!("ingest-{n}.sst"));
                let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
                for k in 0..KEYS {
                    writer
                        .put(format!("ingested/{n}/{k:03}").as_bytes(), b"in")
                        .unwrap();
                }
                writer.finish().unwrap();
                db.ingest_external_files(&[source], IngestOptions::default())
                    .wait()
                    .unwrap();
            }
        });
        let readers: Vec<_> = (0..3u64)
            .map(|r| {
                let (db, done, reads) = (&db, &done, &reads);
                scope.spawn(move || {
                    let mut seen = vec![0u64; (WRITERS * KEYS) as usize];
                    let mut i = r;
                    while !done.load(Ordering::Acquire) {
                        let (w, k) = (i % WRITERS, (i / WRITERS) % KEYS);
                        if let Some(value) = db.get(&key(w, k)).unwrap() {
                            let version = u64::from_be_bytes(value[..8].try_into().unwrap());
                            let slot = &mut seen[(w * KEYS + k) as usize];
                            assert!(
                                version >= *slot,
                                "a read of w{w}/{k:03} went back from {slot} to {version}"
                            );
                            *slot = version;
                        }
                        reads.fetch_add(1, Ordering::Relaxed);
                        i = i.wrapping_add(7);
                    }
                })
            })
            .collect();
        // Every writer's and the ingest's outcome first, then the readers
        // are stopped, so a writer that fails is reported rather than
        // leaving the readers spinning.
        let wrote: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        let ingested = ingest.join();
        done.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
        for outcome in wrote {
            outcome.unwrap();
        }
        ingested.unwrap();
    });
    assert!(reads.load(Ordering::Relaxed) > 0);
    db.compact_range(None, None).wait().unwrap();
    for w in 0..WRITERS {
        for k in 0..KEYS {
            let value = db.get(&key(w, k)).unwrap().expect("a write was lost");
            assert_eq!(u64::from_be_bytes(value[..8].try_into().unwrap()), VERSIONS);
        }
    }
    for n in 0..INGESTS {
        for k in 0..KEYS {
            assert_eq!(
                db.get(format!("ingested/{n}/{k:03}").as_bytes()).unwrap(),
                Some(b"in".to_vec()),
                "an ingested table was lost"
            );
        }
    }
    db.close().unwrap();
}

/// The worker flushes and compacts, rotations seal under the pipeline mutex
/// and run the flush themselves when the worker falls behind, and ingests
/// install tables, all publishing the read view at once.
#[test]
fn every_view_publisher_racing_readers_loses_no_write_with_a_worker() {
    let ingest_dir = TempDir::new().unwrap();
    publishers_racing_readers(
        Options::default()
            .write_buffer_size(4 * 1024)
            .max_background_compactions(1),
        ingest_dir.path(),
    );
}

/// With no worker, every flush and compaction is a step a writer runs after
/// its commit, or the compaction a stopped writer runs to relieve the stall
/// (inline compaction): those publish the read view too.
#[test]
fn every_view_publisher_racing_readers_loses_no_write_without_a_worker() {
    let ingest_dir = TempDir::new().unwrap();
    publishers_racing_readers(
        Options::default()
            .write_buffer_size(4 * 1024)
            .max_background_compactions(0)
            .inline_compaction(true)
            .level0_slowdown_writes_trigger(2)
            .l0_compaction_trigger(3),
        ingest_dir.path(),
    );
}

// Snapshot slots, the read view and compaction ------------------------------

/// Snapshots pinned on writer threads at known versions, handed to other
/// threads, read there through flushes and compactions that fold the
/// versions between them, and dropped there. Each reads the version it was
/// pinned at; once the last drops, nothing is pinned and the old versions go.
#[test]
fn snapshots_moved_across_threads_keep_their_versions_through_compaction() {
    const ROUNDS: u64 = 30;
    let dir = TempDir::new().unwrap();
    let db = Db::open(
        dir.path(),
        Options::default()
            .write_buffer_size(16 * 1024)
            .max_background_compactions(1),
    )
    .unwrap();
    let (send, receive) = std::sync::mpsc::channel::<(u64, regolith::Snapshot)>();
    std::thread::scope(|scope| {
        let db = &db;
        let pinner = scope.spawn(move || {
            for v in 0..ROUNDS {
                for k in 0..20u64 {
                    let mut value = v.to_be_bytes().to_vec();
                    value.resize(256, b'f');
                    through_stalls(|| db.put(&numbered(k), &value)).unwrap();
                }
                send.send((v, db.snapshot())).unwrap();
                if v % 5 == 4 {
                    db.flush().unwrap();
                }
            }
            drop(send);
        });
        let holder = scope.spawn(move || {
            let mut held = Vec::new();
            while let Ok((v, snapshot)) = receive.recv() {
                held.push((v, snapshot));
                if held.len() % 7 == 0 {
                    db.compact_range(None, None).wait().unwrap();
                }
                for (v, snapshot) in &held {
                    let value = snapshot.get(&numbered(*v % 20)).unwrap().unwrap();
                    assert_eq!(
                        u64::from_be_bytes(value[..8].try_into().unwrap()),
                        *v,
                        "a snapshot moved to another thread lost its version"
                    );
                }
                // Drop every other one here, on the thread it was moved to.
                if held.len() > 3 {
                    held.remove(0);
                }
            }
            held
        });
        pinner.join().unwrap();
        let held = holder.join().unwrap();
        assert!(!held.is_empty());
        // The rest drop on yet another thread while this one waits on them.
        let waiter = scope.spawn(|| db.wait_for_snapshots().wait(Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(20));
        drop(held);
        assert_eq!(waiter.join().unwrap(), 0, "the wait missed a release");
    });
    assert_eq!(db.get_int_property("regolith.num-snapshots"), Some(0));
    db.compact_range(None, None).wait().unwrap();
    for k in 0..20u64 {
        let value = db.get(&numbered(k)).unwrap().unwrap();
        assert_eq!(
            u64::from_be_bytes(value[..8].try_into().unwrap()),
            ROUNDS - 1
        );
    }
}

// The block cache and sealed blocks -----------------------------------------

/// A cache of one 64 KiB shard on an encrypted database.
fn small_sealed_cache(stats: &Arc<Statistics>) -> Options {
    Options::default()
        .key_provider(Keys::new(&[1]))
        .block_size(1024)
        .block_cache_size(64 * 1024)
        .block_cache_num_shard_bits(0)
        .statistics(Some(Arc::clone(stats)))
}

/// Readers hold a slice of every block the cache can keep. A blocking read
/// of another sealed block opens it, is refused a place in the cache, counts
/// the refusal and still answers. A queued read of a sealed block the cache
/// refuses lands in its queue, and the re-run reads it from the landing.
#[test]
fn a_cache_full_of_held_sealed_blocks_refuses_counts_and_queued_reads_still_land() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    {
        let db = Db::open(dir.path(), small_sealed_cache(&stats)).unwrap();
        for i in 0..4_000u64 {
            through_stalls(|| db.put(&numbered(i), &[b'v'; 200])).unwrap();
        }
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), small_sealed_cache(&stats)).unwrap();
    // Hold one slice per block, about five keys a block, until a read finds
    // the cache refusing.
    let mut held: Vec<DbSlice> = Vec::new();
    let mut i = 0u64;
    let refused = || stats.get_ticker(Ticker::BlockCacheAddRefusedHeld);
    let before = refused();
    while refused() == before {
        assert!(i < 4_000, "setup: the held slices never filled the cache");
        held.push(db.get_slice(&numbered(i)).unwrap().unwrap());
        i += 5;
    }
    let read_past = i;
    // A blocking read past the held blocks still answers, uncached.
    let far = 3_990;
    assert_eq!(db.get(&numbered(far)).unwrap(), Some(vec![b'v'; 200]));
    // A queued read: the unit opens the sealed block, the cache refuses it,
    // and the queue's landing holds it for the re-run.
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let target = (read_past + 1_000).min(3_980);
    let counted = refused();
    assert_eq!(
        through_queue(&mut queue, || snapshot.get(&numbered(target))).unwrap(),
        Some(vec![b'v'; 200])
    );
    assert!(
        refused() > counted,
        "the queued read's block was cached: the landing was not what answered"
    );
    // Every held slice still reads its own bytes.
    for slice in &held {
        assert_eq!(&slice[..], &[b'v'; 200][..]);
    }
    drop(held);
    // With the holds gone the cache admits again.
    let counted = refused();
    for k in (0..400).step_by(5) {
        db.get(&numbered(k)).unwrap();
    }
    assert_eq!(
        refused(),
        counted,
        "an insert was refused with nothing held"
    );
}

/// Queued reads of sealed blocks wait on their queue while readers hold
/// every block the cache can keep, then the database closes: close and the
/// misses meet at one gate, so every wait completes, the queue owes nothing,
/// and the reads run again answer `Closed`.
#[test]
fn close_completes_queued_reads_of_sealed_blocks_the_cache_cannot_keep() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    {
        let db = Db::open(dir.path(), small_sealed_cache(&stats)).unwrap();
        for i in 0..4_000u64 {
            through_stalls(|| db.put(&numbered(i), &[b'v'; 200])).unwrap();
        }
        db.flush().unwrap();
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
    }
    let db = Db::open(dir.path(), small_sealed_cache(&stats)).unwrap();
    let refused = || stats.get_ticker(Ticker::BlockCacheAddRefusedHeld);
    let before = refused();
    let mut held: Vec<DbSlice> = Vec::new();
    let mut i = 0u64;
    while refused() == before {
        held.push(db.get_slice(&numbered(i)).unwrap().unwrap());
        i += 5;
    }
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let waits: Vec<regolith::IoWait> = (i + 500..4_000)
        .step_by(500)
        .map(|k| match snapshot.get(&numbered(k)) {
            Err(Error::WouldBlock(regolith::WouldBlock::Io(wait))) => wait,
            other => panic!("expected the read of key {k} to wait, got {other:?}"),
        })
        .collect();
    assert!(!waits.is_empty());
    db.close().unwrap();
    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, waits.len(), "every wait completed");
    assert!(!progress.more_pending, "the queue owes nothing after close");
    assert!(waits.iter().all(regolith::IoWait::is_ready));
    assert!(matches!(
        snapshot.get(&numbered(i + 500)),
        Err(Error::Closed)
    ));
    drop(held);
}

// The open-file table, encryption and ingest ---------------------------------

/// Many sealed tables, one ingested table stamped above them, two
/// descriptors and no block cache: every read reopens descriptors in the
/// slot table, blocking and through a queue, and reads each sealed table
/// and the ingested one at its sequence.
#[test]
fn a_hard_open_file_limit_reads_sealed_tables_and_an_ingest_at_its_sequence() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().with_extension("ingest.sst");
    let options = || {
        Options::default()
            .key_provider(Keys::new(&[1]))
            .max_open_files(2)
            .block_cache_size(0)
            .l0_compaction_trigger(64)
            .max_background_compactions(0)
    };
    {
        let db = Db::open(dir.path(), options()).unwrap();
        for table in 0..8u64 {
            for k in 0..50 {
                regolith::through_stalls(|| {
                    db.put(&numbered(table * 50 + k), format!("t{table}").as_bytes())
                })
                .unwrap();
            }
            db.flush().unwrap();
        }
        // Overwrites every tenth key of every table, at one sequence above
        // them all.
        let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
        for i in (0..400).step_by(10) {
            writer.put(&numbered(i), b"ingested").unwrap();
        }
        writer.finish().unwrap();
        db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default())
            .wait()
            .unwrap();
        db.close().unwrap();
    }
    let expected = |i: u64| -> Vec<u8> {
        if i.is_multiple_of(10) {
            b"ingested".to_vec()
        } else {
            format!("t{}", i / 50).into_bytes()
        }
    };
    let db = Db::open(dir.path(), options()).unwrap();
    std::thread::scope(|scope| {
        for t in 0..4u64 {
            let db = &db;
            scope.spawn(move || {
                for i in (t..400).step_by(4) {
                    assert_eq!(db.get(&numbered(i)).unwrap(), Some(expected(i)), "key {i}");
                }
            });
        }
    });
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    for i in (3..400).step_by(37) {
        assert_eq!(
            through_queue(&mut queue, || snapshot.get(&numbered(i))).unwrap(),
            Some(expected(i)),
            "queued read of key {i}"
        );
    }
    let scanned = through_queue(&mut queue, || snapshot.scan(None, None)).unwrap();
    assert_eq!(scanned.len(), 400);
    for (i, (key, value)) in scanned.iter().enumerate() {
        assert_eq!(key, &numbered(i as u64));
        assert_eq!(value, &expected(i as u64), "scanned key {i}");
    }
}

// Tables renamed aside, backups and the sweep --------------------------------

const REMOVED: &str = ".removed-";

/// A compaction removes tables a held iterator still reads; under the hard
/// descriptor bound they are renamed aside. A checkpoint, a backup and its
/// restore copy only the tables the version names, and each opens with every
/// key. While the reader lives no second open can sweep under it; once it
/// drops, its last handle unlinks what was renamed aside.
#[test]
fn backups_and_checkpoints_never_copy_a_table_renamed_aside() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("db");
    let options = || {
        Options::default()
            .max_open_files(3)
            .block_cache_size(0)
            .max_background_compactions(0)
    };
    let db = Db::open(&dir, options()).unwrap();
    for table in 0..4u64 {
        for k in 0..50 {
            regolith::through_stalls(|| db.put(&numbered(table * 50 + k), b"before")).unwrap();
        }
        db.flush().unwrap();
    }
    // The iterator holds the version it was made on, and with it a live
    // handle on every one of the four tables.
    let mut held = db.iter();
    held.seek_to_first();
    assert_eq!(held.value(), Some(&b"before"[..]));
    let tables: Vec<String> = table_dir_files(&dir, ".sst")
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(tables.len(), 4);
    for i in 0..200 {
        regolith::through_stalls(|| db.put(&numbered(i), b"after")).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    // The four the iterator holds; the table the compaction flushed first is
    // held only by views the reclaimer has yet to free, and may go any time.
    let aside: Vec<PathBuf> = table_dir_files(&dir, REMOVED)
        .into_iter()
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            tables
                .iter()
                .any(|t| name.starts_with(&format!("{t}{REMOVED}")))
        })
        .collect();
    assert_eq!(
        aside.len(),
        4,
        "setup: the compaction renamed each table the iterator holds aside"
    );

    let checkpoint = root.path().join("checkpoint");
    Checkpoint::new(&db)
        .unwrap()
        .create(&checkpoint)
        .wait()
        .unwrap();
    let backups = root.path().join("backups");
    let mut engine = BackupEngine::open(&backups).unwrap();
    let id = engine.create_backup(&db).unwrap();
    let restored = root.path().join("restored");
    engine.restore(id, &restored, None).unwrap();
    for copy in [&checkpoint, &backups, &restored] {
        let names = all_names(copy);
        assert!(
            !names.iter().any(|n| n.contains(REMOVED)),
            "{}: a table renamed aside was copied: {names:?}",
            copy.display()
        );
    }
    for copy in [&checkpoint, &restored] {
        let opened = Db::open(copy, options()).unwrap();
        for i in 0..200 {
            assert_eq!(opened.get(&numbered(i)).unwrap(), Some(b"after".to_vec()));
        }
        opened.close().unwrap();
    }

    // While the reader lives the directory stays locked: no second open,
    // and so no sweep, can run under it.
    assert!(
        Db::open(&dir, options()).is_err(),
        "a second open of a database a reader still uses succeeded"
    );
    for path in &aside {
        assert!(path.exists(), "a table a reader holds was removed");
    }
    // The reader reads the tables renamed aside to the end, reopening
    // descriptors under their new names as it goes.
    let mut read = 1;
    held.next();
    while held.valid() {
        assert_eq!(held.value(), Some(&b"before"[..]));
        read += 1;
        held.next();
    }
    held.status().unwrap();
    assert_eq!(read, 200);
    drop(held);
    wait_until("the last reader unlinks the tables renamed aside", || {
        // A read on this thread moves reclamation on, so the views that
        // named the tables are freed.
        db.get(&numbered(0)).unwrap();
        table_dir_files(&dir, REMOVED).is_empty()
    });
    db.close().unwrap();
    drop(db);

    // One a crash left behind is swept by the next writable open.
    let left = dir.join("sst").join("000002.sst.removed-99");
    std::fs::write(&left, b"left by a crash").unwrap();
    let db = Db::open(&dir, options()).unwrap();
    assert!(!left.exists(), "the open left a table renamed aside");
    assert_eq!(db.get(&numbered(0)).unwrap(), Some(b"after".to_vec()));
}

// The commit side --------------------------------------------------------------

/// Writers commit through `commit_nowait` on queues of their own at
/// `Immediate` durability, with no worker: their groups are left owing their
/// fsync and land at a poll, and the flushes and compaction steps their
/// writes owe run as jobs on their queues. Readers on other threads never see
/// a key go back, and every write lands.
#[test]
fn nowait_commits_and_owed_steps_on_queues_lose_no_write_under_readers() {
    const WRITERS: u64 = 3;
    const KEYS: u64 = 20;
    const VERSIONS: u64 = 15;
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(
        dir.path(),
        Options::default()
            .write_buffer_size(4 * 1024)
            .max_background_compactions(0)
            .inline_compaction(true)
            .durability(DurabilityMode::Immediate),
    )
    .unwrap();
    let key = |w: u64, k: u64| format!("w{w}/{k:03}").into_bytes();
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                let db = &db;
                scope.spawn(move || {
                    // Made on this thread, so the I/O its writes owe is left
                    // here and run at its polls.
                    let mut queue = db.db().io_queue();
                    for v in 1..=VERSIONS {
                        for k in 0..KEYS {
                            let mut value = v.to_be_bytes().to_vec();
                            value.resize(256, b'n');
                            loop {
                                let txn = db.begin(&TxnOptions::new().io_queue(queue.id()));
                                txn.put(&key(w, k), &value).unwrap();
                                match queue.block_on(txn.commit_nowait()) {
                                    Ok(_) => break,
                                    Err(TransactionError::WouldBlock(stall)) => {
                                        let _ = stall;
                                        queue.poll(IoBudget::ALL);
                                    }
                                    Err(other) => panic!("a nowait commit failed: {other}"),
                                }
                            }
                        }
                    }
                })
            })
            .collect();
        let readers: Vec<_> = (0..2u64)
            .map(|r| {
                let (db, done) = (&db, &done);
                scope.spawn(move || {
                    let mut seen = vec![0u64; (WRITERS * KEYS) as usize];
                    let mut i = r;
                    while !done.load(Ordering::Acquire) {
                        let (w, k) = (i % WRITERS, (i / WRITERS) % KEYS);
                        if let Some(value) = db.db().get(&key(w, k)).unwrap() {
                            let version = u64::from_be_bytes(value[..8].try_into().unwrap());
                            let slot = &mut seen[(w * KEYS + k) as usize];
                            assert!(version >= *slot, "a read of w{w}/{k:03} went back");
                            *slot = version;
                        }
                        i = i.wrapping_add(5);
                    }
                })
            })
            .collect();
        let wrote: Vec<_> = writers.into_iter().map(|w| w.join()).collect();
        done.store(true, Ordering::Release);
        for reader in readers {
            reader.join().unwrap();
        }
        for outcome in wrote {
            outcome.unwrap();
        }
    });
    for w in 0..WRITERS {
        for k in 0..KEYS {
            let value = db.db().get(&key(w, k)).unwrap().expect("a write was lost");
            assert_eq!(u64::from_be_bytes(value[..8].try_into().unwrap()), VERSIONS);
        }
    }
}

/// A `transact_async` attempt suspends on cache misses and resumes at the
/// queue's polls; the first attempt then loses a race and the closure runs
/// again. While suspended, exactly the running attempt's snapshot is pinned;
/// once the future resolves, every attempt's pin was released exactly once.
#[test]
fn transact_async_attempts_pin_one_snapshot_each_and_release_it_once() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let options = || {
        Options::default()
            .block_size(256)
            .statistics(Some(Arc::clone(&stats)))
    };
    {
        let db = Db::open(dir.path(), options()).unwrap();
        for i in 0..2_000u64 {
            regolith::through_stalls(|| db.put(&numbered(i), b"cold")).unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }
    // Reopened, so the reads below miss the cache and suspend.
    let db = OptimisticTransactionDb::open(dir.path(), options()).unwrap();
    let mut queue = db.db().io_queue();
    let registered = || stats.get_ticker(Ticker::SnapshotsRegistered);
    let released = || stats.get_ticker(Ticker::SnapshotsReleased);
    let (reg0, rel0) = (registered(), released());
    let attempts = std::cell::Cell::new(0u64);
    let policy = RetryPolicy::new(4);
    let future = db.transact_async(
        queue.id(),
        &policy,
        async |txn: &mut regolith::Transaction, previous: Option<&regolith::Conflict>| {
            attempts.set(attempts.get() + 1);
            let first = ready(|| txn.get_slice(&numbered(10))).await?;
            let second = ready(|| txn.get_slice(&numbered(1_900))).await?;
            assert!(first.is_some() && second.is_some());
            if previous.is_none() {
                // Another writer changes what the attempt read: it conflicts.
                regolith::through_stalls(|| db.db().put(&numbered(10), b"raced")).unwrap();
            }
            txn.put(&numbered(10), b"mine")?;
            Ok::<_, TransactionError>(())
        },
    );
    let mut future = std::pin::pin!(future);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut suspensions = 0;
    let outcome = loop {
        match future.as_mut().poll(&mut cx) {
            std::task::Poll::Ready(outcome) => break outcome,
            std::task::Poll::Pending => {
                suspensions += 1;
                assert_eq!(
                    db.db().get_int_property("regolith.num-snapshots"),
                    Some(1),
                    "a suspended attempt pins its own snapshot and no other"
                );
                queue.poll(IoBudget::ALL);
                assert!(suspensions < 10_000, "the attempt never finished");
            }
        }
    };
    outcome.expect("the transaction committed on its second attempt");
    assert!(suspensions > 0, "setup: the attempt suspended on a miss");
    assert_eq!(attempts.get(), 2, "setup: the first attempt lost the race");
    assert_eq!(registered() - reg0, 2, "one pin per attempt");
    assert_eq!(released() - rel0, 2, "every attempt's pin released once");
    assert_eq!(db.db().get_int_property("regolith.num-snapshots"), Some(0));
    assert_eq!(db.db().get(&numbered(10)).unwrap(), Some(b"mine".to_vec()));
}

/// A checkpoint run as a job (on the compaction worker, or with no worker on
/// the caller's queue), while a held iterator keeps tables a compaction
/// renamed aside under a hard open-file bound: the checkpoint copies only the
/// tables the version names, and opens with every key.
fn checkpoint_job_under_an_open_file_bound(workers: usize) {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("db");
    let options = || {
        Options::default()
            .max_open_files(3)
            .block_cache_size(0)
            .max_background_compactions(workers)
            .l0_compaction_trigger(64)
    };
    let db = Db::open(&dir, options()).unwrap();
    let mut queue = db.io_queue();
    for table in 0..4u64 {
        for k in 0..50 {
            regolith::through_stalls(|| db.put(&numbered(table * 50 + k), b"before")).unwrap();
        }
        db.flush().unwrap();
    }
    let mut held = db.iter();
    held.seek_to_first();
    for i in 0..200 {
        regolith::through_stalls(|| db.put(&numbered(i), b"after")).unwrap();
    }
    db.compact_range(None, None).wait().unwrap();
    assert!(
        !table_dir_files(&dir, REMOVED).is_empty(),
        "setup: the compaction renamed the tables the iterator holds aside"
    );
    let checkpoint = root.path().join("checkpoint");
    let ticket = Checkpoint::new(&db).unwrap().create(&checkpoint);
    // With no worker the job waits on this thread's queue for its poll.
    queue.block_on(ticket).unwrap();
    let names = all_names(&checkpoint);
    assert!(
        !names.iter().any(|n| n.contains(REMOVED)),
        "a table renamed aside was copied: {names:?}"
    );
    let opened = Db::open(&checkpoint, options()).unwrap();
    for i in 0..200 {
        assert_eq!(opened.get(&numbered(i)).unwrap(), Some(b"after".to_vec()));
    }
    let mut read = 0;
    while held.valid() {
        assert_eq!(held.value(), Some(&b"before"[..]));
        read += 1;
        held.next();
    }
    held.status().unwrap();
    assert_eq!(read, 200);
}

#[test]
fn a_checkpoint_job_on_the_worker_under_an_open_file_bound_copies_only_live_tables() {
    checkpoint_job_under_an_open_file_bound(1);
}

#[test]
fn a_checkpoint_job_on_the_callers_queue_under_an_open_file_bound_copies_only_live_tables() {
    checkpoint_job_under_an_open_file_bound(0);
}

/// Holds a compaction at its start until told to go on, and says when one
/// began.
struct HoldCompaction {
    held: AtomicBool,
    began: std::sync::mpsc::SyncSender<()>,
    go_on: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl regolith::EventListener for HoldCompaction {
    fn on_compaction_begin(&self, _: &regolith::CompactionJobInfo) {
        // Only the first compaction is held.
        if !self.held.swap(true, Ordering::AcqRel) && self.began.try_send(()).is_ok() {
            // Bounded: a test that never says go on fails on its own wait.
            let _ = self
                .go_on
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30));
        }
    }
}

/// A database dropped while its compaction worker is inside a job the caller
/// did not wait for. The worker holds the engine until the job ends, so the
/// engine is torn down on the worker's own thread: that must stop the other
/// workers, join no thread with itself, and release the directory, which
/// then opens again with every write.
#[test]
fn a_database_dropped_under_a_running_job_releases_its_directory_and_reopens() {
    // A panic on one of the database's own threads, which no join reports.
    static WORKER_PANICKED: AtomicBool = AtomicBool::new(false);
    let earlier = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current()
            .name()
            .is_some_and(|name| name.starts_with("regolith-"))
        {
            WORKER_PANICKED.store(true, Ordering::Release);
        }
        earlier(info);
    }));

    let dir = TempDir::new().unwrap();
    let (began_tx, began) = std::sync::mpsc::sync_channel(1);
    let (go_on, go_on_rx) = std::sync::mpsc::channel();
    let hold = Arc::new(HoldCompaction {
        held: AtomicBool::new(false),
        began: began_tx,
        go_on: std::sync::Mutex::new(go_on_rx),
    });
    // No pass starts by itself: the only compaction is the job's.
    let options = || {
        Options::default()
            .write_buffer_size(64 << 20)
            .max_background_compactions(2)
            .l0_compaction_trigger(1_000)
            .level0_slowdown_writes_trigger(0)
            .level0_stop_writes_trigger(0)
    };
    let db = Db::open(
        dir.path(),
        options().listeners(vec![hold as Arc<dyn regolith::EventListener>]),
    )
    .unwrap();
    for i in 0..400u64 {
        regolith::through_stalls(|| db.put(&numbered(i), &[7u8; 256])).unwrap();
        // Several level 0 tables, so the job has a compaction to run.
        if i % 100 == 99 {
            db.flush().unwrap();
        }
    }
    // The ticket is dropped: the job runs on, on a worker.
    drop(db.compact_range(None, None));
    began
        .recv_timeout(Duration::from_secs(30))
        .expect("the compaction began");
    drop(db);
    go_on.send(()).unwrap();

    let mut reopened = None;
    wait_until("the dropped database to release its directory", || {
        reopened = Db::open(dir.path(), options()).ok();
        reopened.is_some()
    });
    let db = reopened.unwrap();
    assert!(
        !WORKER_PANICKED.load(Ordering::Acquire),
        "a worker panicked tearing the database down"
    );
    for i in 0..400u64 {
        assert!(db.get(&numbered(i)).unwrap().is_some(), "key {i}");
    }
}
