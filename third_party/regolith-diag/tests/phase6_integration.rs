//! Where the Phase 6 packages meet, tested across them.
//!
//! - **Log retirement on every flush path** (#268 x #265). Group commit moved
//!   the flush off the commit path: the compaction worker flushes, a writer
//!   with no worker flushes after its commit returns, and a writer stopped by
//!   a stall flushes inside its bounded step. Each of them must record
//!   `min_wal_id` in the table's batch and retire the log through
//!   `RetiredLogs`, so a log whose removal failed is reported, retried, and
//!   never replayed above a newer table.
//! - **A restore and the retired logs** (#266's sealed backups x #268). A
//!   restored database holds what its backup holds: its MANIFEST records
//!   every log below the backup's next file id as in tables, so no log the
//!   restore target held is replayed into it.
//! - **Queue reads of sealed tables** (#269 x #266). The read a queue runs
//!   opens a sealed block before it lands, applies an ingested table's
//!   sequence, and reports a block that fails its tag once, naming the table.
//!   Every read path on an encrypted database is in `io_queue_paths.rs`.
//! - **Queue reads and close** (#269 x #265). Close releases the units reads
//!   wait on, then flushes and joins the worker, while reads already under
//!   way and the worker's flushes go on: every wait completes, no unit is
//!   left, and a close that fails leaves queue reads working.
//! - **Log retirement under a hard open-file bound** (#268 x Phase 7c1). The
//!   open-file table renames a removed table aside while a reader holds it,
//!   and passes every other file through, so a log's removal, its failure
//!   and its retry behave on every flush path exactly as without the bound.
#![cfg(not(target_arch = "wasm32"))]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::faulty_env::{FaultyEnv, Refuse, logs};
use common::keys::Keys;
use regolith::env::Env;
use regolith::{
    BackupEngine, Db, Error, IngestOptions, IoBudget, IoQueue, IoWait, Options, ReadMode,
    SstFileWriter, Statistics, Ticker, WouldBlock,
};
use tempfile::TempDir;

/// Bytes of filler each write carries, so a few writes fill a memtable.
const FILLER: usize = 1024;
/// The write buffer the flush tests run with.
const WRITE_BUFFER: usize = 8 * 1024;

/// The three threads that flush a sealed memtable.
#[derive(Clone, Copy, Debug)]
enum FlushPath {
    /// The compaction worker, woken by the seal.
    Worker,
    /// The writer whose commit sealed it, once that commit returned, on a
    /// database with no worker.
    AfterCommit,
    /// A writer slowed by a stall, in the bounded step its slowed write owes
    /// on a database with no worker and inline compaction on, run once its
    /// commit returned.
    StallStep,
}

struct Fixture {
    dir: TempDir,
    env: Arc<FaultyEnv>,
    stats: Arc<Statistics>,
    path: FlushPath,
    filler: usize,
    /// `Options::max_open_files`; 0 for no bound.
    open_files: usize,
}

impl Fixture {
    fn new(path: FlushPath) -> Self {
        Self {
            dir: TempDir::new().unwrap(),
            env: Arc::new(FaultyEnv::default()),
            stats: Arc::new(Statistics::new()),
            path,
            filler: 0,
            open_files: 0,
        }
    }

    /// The same fixture under a hard bound of `n` open tables.
    fn open_files(mut self, n: usize) -> Self {
        self.open_files = n;
        self
    }

    fn options(&self) -> Options {
        let options = Options::default()
            .env(Arc::clone(&self.env) as Arc<dyn Env>)
            .statistics(Some(Arc::clone(&self.stats)))
            .write_buffer_size(WRITE_BUFFER)
            .max_open_files(self.open_files);
        match self.path {
            FlushPath::Worker => options.max_background_compactions(1),
            FlushPath::AfterCommit => options.max_background_compactions(0),
            // With one table at L0 every write is slowed down, and a slowed
            // write with no worker and inline compaction on owes one bounded
            // step, run once its commit returned: the flush of the oldest
            // frozen memtable when there is one. The compaction trigger is
            // out of reach, so the steps between flushes find nothing to
            // compact.
            FlushPath::StallStep => options
                .max_background_compactions(0)
                .inline_compaction(true)
                .level0_slowdown_writes_trigger(1)
                .l0_compaction_trigger(64),
        }
    }

    fn open(&self) -> Db {
        Db::open(self.dir.path(), self.options()).unwrap()
    }

    fn ticker(&self, ticker: Ticker) -> u64 {
        self.stats.get_ticker(ticker)
    }

    fn frozen_bytes(db: &Db) -> u64 {
        db.get_int_property("regolith.num-entries-imm-mem-tables")
            .unwrap()
    }

    fn fill(&mut self, db: &Db) {
        let key = format!("filler/{:06}", self.filler);
        self.filler += 1;
        regolith::through_stalls(|| db.put(key.as_bytes(), &[b'f'; FILLER])).unwrap();
    }

    /// Put `key = value`, then have this fixture's path flush the memtable
    /// holding it. Returns once that flush has run.
    fn put_and_flush(&mut self, db: &Db, key: &[u8], value: &[u8]) {
        let flushes = self.ticker(Ticker::FlushCount);
        regolith::through_stalls(|| db.put(key, value)).unwrap();
        match self.path {
            FlushPath::Worker => {
                // Write until the memtable seals, then stop: a second seal
                // would have the rotation flush inline instead.
                while Self::frozen_bytes(db) == 0 && self.ticker(Ticker::FlushCount) == flushes {
                    self.fill(db);
                }
                wait_until("the worker flushes the sealed memtable", || {
                    self.ticker(Ticker::FlushCount) > flushes
                });
            }
            FlushPath::AfterCommit => {
                while self.ticker(Ticker::FlushCount) == flushes {
                    self.fill(db);
                }
            }
            FlushPath::StallStep => {
                // The flush the sealing writer owes fails, so the memtable is
                // still frozen when the next write runs its stall step.
                self.env.fail_tables(true);
                while Self::frozen_bytes(db) == 0 {
                    self.fill(db);
                }
                self.env.fail_tables(false);
                assert!(self.env.tables_refused.load(Ordering::SeqCst) > 0);
                assert_eq!(self.ticker(Ticker::FlushCount), flushes);
                self.fill(db);
                assert_eq!(
                    Self::frozen_bytes(db),
                    0,
                    "the stall step flushed the frozen memtable"
                );
            }
        }
        assert_eq!(
            self.ticker(Ticker::FlushCount),
            flushes + 1,
            "{:?}: exactly one flush ran",
            self.path
        );
    }
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

/// The one log in the database now: the active one.
fn active_log(db: &Path) -> PathBuf {
    let logs = logs(db);
    assert_eq!(logs.len(), 1, "one log before the flush: {logs:?}");
    logs[0].clone()
}

/// On `path`: the flush of `k = old` cannot remove its log, the flush of
/// `k = new` retries that removal and fails again, and the reopen reads
/// `new`. Each failure is counted, and the reopen removes the leftover.
fn a_log_left_behind_is_counted_and_never_replayed(path: FlushPath) {
    a_log_left_behind_is_counted_and_never_replayed_in(Fixture::new(path));
}

fn a_log_left_behind_is_counted_and_never_replayed_in(mut f: Fixture) {
    let path = f.path;
    let leftover = {
        let db = f.open();
        if let FlushPath::StallStep = path {
            // The table that turns the slowdown on.
            regolith::through_stalls(|| db.put(b"seed", b"v")).unwrap();
            db.flush().unwrap();
        }
        let leftover = active_log(f.dir.path());
        f.env.arm(Refuse::Log(leftover.clone()));
        f.put_and_flush(&db, b"k", b"old");
        assert_eq!(
            f.ticker(Ticker::WalRemoveFailed),
            1,
            "{path:?}: the flush reported the log it could not remove"
        );
        assert!(leftover.exists(), "{path:?}: the log outlived its flush");

        f.put_and_flush(&db, b"k", b"new");
        assert_eq!(
            f.ticker(Ticker::WalRemoveFailed),
            2,
            "{path:?}: the next flush retried the removal"
        );
        assert_eq!(db.get(b"k").unwrap().as_deref(), Some(&b"new"[..]));
        db.close().unwrap();
        leftover
    };
    f.env.arm(Refuse::Nothing);
    assert!(
        leftover.exists(),
        "{path:?}: the leftover reaches the reopen"
    );

    let db = f.open();
    assert_eq!(
        db.get(b"k").unwrap().as_deref(),
        Some(&b"new"[..]),
        "{path:?}: recovery replayed a flushed log above the newer table"
    );
    assert!(
        !leftover.exists(),
        "{path:?}: the open removed the leftover"
    );
}

#[test]
fn the_worker_flush_retires_its_log_and_records_it_flushed() {
    a_log_left_behind_is_counted_and_never_replayed(FlushPath::Worker);
}

#[test]
fn the_flush_after_a_commit_retires_its_log_and_records_it_flushed() {
    a_log_left_behind_is_counted_and_never_replayed(FlushPath::AfterCommit);
}

#[test]
fn the_stall_step_flush_retires_its_log_and_records_it_flushed() {
    a_log_left_behind_is_counted_and_never_replayed(FlushPath::StallStep);
}

#[test]
fn under_a_hard_open_file_bound_the_worker_flush_retires_its_log() {
    a_log_left_behind_is_counted_and_never_replayed_in(
        Fixture::new(FlushPath::Worker).open_files(2),
    );
}

#[test]
fn under_a_hard_open_file_bound_the_flush_after_a_commit_retires_its_log() {
    a_log_left_behind_is_counted_and_never_replayed_in(
        Fixture::new(FlushPath::AfterCommit).open_files(2),
    );
}

#[test]
fn under_a_hard_open_file_bound_the_stall_step_flush_retires_its_log() {
    a_log_left_behind_is_counted_and_never_replayed_in(
        Fixture::new(FlushPath::StallStep).open_files(2),
    );
}

#[test]
fn a_restored_database_replays_no_log_its_target_held() {
    let source = TempDir::new().unwrap();
    let db = Db::open(source.path(), Options::default()).unwrap();
    regolith::through_stalls(|| db.put(b"k", b"backed-up")).unwrap();
    let backups = TempDir::new().unwrap();
    let mut engine = BackupEngine::open(backups.path()).unwrap();
    let id = engine.create_backup(&db).unwrap();
    // Writes after the backup, held only by the log the backup's flush
    // started, whose number is below the backup's next file id.
    regolith::through_stalls(|| db.put(b"k", b"after")).unwrap();
    regolith::through_stalls(|| db.put(b"only-in-a-log", b"v")).unwrap();
    let log = logs(source.path()).pop().expect("the source's active log");
    drop(db);

    let root = TempDir::new().unwrap();
    let target = root.path().join("restored");
    std::fs::create_dir_all(target.join("wal")).unwrap();
    let stray = target.join("wal").join(log.file_name().unwrap());
    std::fs::copy(&log, &stray).unwrap();
    engine.restore(id, &target, None).unwrap();

    let restored = Db::open(&target, Options::default()).unwrap();
    assert_eq!(
        restored.get(b"k").unwrap().as_deref(),
        Some(&b"backed-up"[..]),
        "the restored database replayed a log its backup does not hold"
    );
    assert_eq!(restored.get(b"only-in-a-log").unwrap(), None);
    assert!(!stray.exists(), "the open removed the log below min_wal_id");
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

fn sealed_options() -> Options {
    Options::default()
        .key_provider(Keys::new(&[1]))
        .block_size(256)
}

fn numbered(i: usize) -> Vec<u8> {
    format!("key/{i:04}").into_bytes()
}

/// The table files of the database at `db`.
fn tables(db: &Path) -> Vec<PathBuf> {
    let mut tables: Vec<PathBuf> = std::fs::read_dir(db.join("sst"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sst"))
        .collect();
    tables.sort();
    tables
}

/// A sealed data block the device returns damaged fails its tag in the read
/// the queue runs. The read run again reports it once, as corruption naming
/// the table; the read after that asks the device again instead of handing
/// out the old failure.
#[test]
fn a_damaged_sealed_block_read_through_a_queue_is_reported_once_naming_the_table() {
    let dir = TempDir::new().unwrap();
    {
        let db = Db::open(dir.path(), sealed_options()).unwrap();
        for i in 0..400 {
            regolith::through_stalls(|| db.put(&numbered(i), format!("value {i}").as_bytes()))
                .unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }
    let table = tables(dir.path()).pop().expect("the flush wrote a table");
    let mut bytes = std::fs::read(&table).unwrap();
    // A third of the way in: among the data blocks, which come first.
    let at = bytes.len() / 3;
    bytes[at] ^= 0x01;
    std::fs::write(&table, &bytes).unwrap();
    let name = table.file_stem().unwrap().to_string_lossy().into_owned();

    let db = Db::open(dir.path(), sealed_options()).unwrap();
    let victim = (0..400)
        .find(|&i| matches!(db.get(&numbered(i)), Err(Error::Corruption(_))))
        .expect("the damage lies in a data block");
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    for round in 0..2 {
        assert!(
            matches!(snapshot.get(&numbered(victim)), Err(Error::WouldBlock(_))),
            "round {round}: the read queues a device read"
        );
        queue.poll(IoBudget::ALL);
        match snapshot.get(&numbered(victim)) {
            Err(Error::Corruption(err)) => {
                let message = err.to_string();
                assert!(
                    message.contains(&format!("table {name}"))
                        && message.contains("does not verify"),
                    "round {round}: {message}"
                );
            }
            other => {
                panic!("round {round}: expected the damaged block's corruption, got {other:?}")
            }
        }
    }
    // A block the damage missed reads through the queue as it reads blocking.
    let fine = (0..400)
        .rev()
        .find(|&i| db.get(&numbered(i)).is_ok())
        .expect("most blocks are whole");
    assert_eq!(
        through_queue(&mut queue, || snapshot.get(&numbered(fine))).unwrap(),
        db.get(&numbered(fine)).unwrap()
    );
}

/// An ingested table's blocks read through a queue on an encrypted database
/// carry the sequence the manifest records for the table, as blocking reads
/// do: a scan merging it with an older table of the same keys yields the
/// ingested versions, which a block read without its stamp would put below
/// the older table's.
#[test]
fn an_ingested_table_read_through_a_queue_on_an_encrypted_database_reads_at_its_sequence() {
    let dir = TempDir::new().unwrap();
    let source = dir.path().with_extension("ingest.sst");
    {
        let db = Db::open(dir.path(), sealed_options()).unwrap();
        for i in 0..200 {
            regolith::through_stalls(|| db.put(&numbered(i), b"old")).unwrap();
        }
        db.flush().unwrap();
        let mut writer = SstFileWriter::create(&source, &Options::default()).unwrap();
        for i in (0..200).step_by(2) {
            writer.put(&numbered(i), b"new").unwrap();
        }
        writer.finish().unwrap();
        db.ingest_external_files(std::slice::from_ref(&source), IngestOptions::default())
            .wait()
            .unwrap();
        db.close().unwrap();
    }
    // Cold: every block the reads below need is read through the queue.
    let db = Db::open(dir.path(), sealed_options()).unwrap();
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let scanned = through_queue(&mut queue, || snapshot.scan(None, None)).unwrap();
    let expected: Vec<(Vec<u8>, Vec<u8>)> = (0..200)
        .map(|i| {
            let value: &[u8] = if i % 2 == 0 { b"new" } else { b"old" };
            (numbered(i), value.to_vec())
        })
        .collect();
    assert_eq!(scanned, expected);
    for i in [0, 1, 98, 99] {
        assert_eq!(
            through_queue(&mut queue, || snapshot.get(&numbered(i))).unwrap(),
            Some(expected[i].1.clone()),
            "key {i}"
        );
    }
}

fn queued_wait(read: regolith::Result<Option<Vec<u8>>>) -> IoWait {
    match read {
        Err(Error::WouldBlock(WouldBlock::Io(wait))) => wait,
        other => panic!("expected the read to wait on its queue, got {other:?}"),
    }
}

/// Close while a writer keeps sealing memtables that the worker flushes off
/// the commit path, with reads waiting on a queue: close's own flush and the
/// worker's race nothing into a lost completion. Every wait completes, the
/// queue owes nothing after, the reads run again answer `Closed`, and every
/// write the writer was told landed survives the reopen.
#[test]
fn close_while_the_worker_flushes_completes_every_queued_read() {
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let options = || {
        Options::default()
            .write_buffer_size(4 * 1024)
            .block_size(256)
            .max_background_compactions(1)
            // The worker only flushes: no compaction fills the cache.
            .l0_compaction_trigger(1000)
            .statistics(Some(Arc::clone(&stats)))
    };
    {
        let db = Db::open(dir.path(), options()).unwrap();
        for i in 0..2000 {
            regolith::through_stalls(|| db.put(&numbered(i), b"in a table")).unwrap();
        }
        db.flush().unwrap();
        db.close().unwrap();
    }
    let db = Arc::new(Db::open(dir.path(), options()).unwrap());
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    let waits: Vec<IoWait> = (125..2000)
        .step_by(250)
        .map(|i| queued_wait(snapshot.get(&numbered(i))))
        .collect();

    let flushes = stats.get_ticker(Ticker::FlushCount);
    let writer = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            let mut landed = 0usize;
            loop {
                match regolith::through_stalls(|| {
                    db.put(format!("late/{landed:06}").as_bytes(), &[b'w'; 512])
                }) {
                    Ok(()) => landed += 1,
                    Err(Error::Closed) => return landed,
                    Err(other) => panic!("a write failed with {other:?}"),
                }
            }
        })
    };
    wait_until("the worker flushes what the writer seals", || {
        stats.get_ticker(Ticker::FlushCount) > flushes + 2
    });
    db.close().unwrap();
    let landed = writer.join().unwrap();

    let progress = queue.poll(IoBudget::ALL);
    assert_eq!(progress.completed, waits.len(), "every wait completed");
    assert!(!progress.more_pending, "the queue owes nothing after close");
    assert!(waits.iter().all(IoWait::is_ready));
    assert!(matches!(snapshot.get(&numbered(0)), Err(Error::Closed)));
    drop((snapshot, queue));
    drop(db);

    let db = Db::open(dir.path(), options()).unwrap();
    for i in 0..landed {
        assert!(
            db.get(format!("late/{i:06}").as_bytes()).unwrap().is_some(),
            "write {i} of {landed}, told it landed, is gone"
        );
    }
}

/// A close whose final flush fails leaves the database open, and reads
/// through a queue opened before it make units again: the close released
/// what waited and marked the queues' table closed, and the failure reopens
/// it.
#[test]
fn a_close_that_fails_leaves_reads_through_queues_working() {
    let dir = TempDir::new().unwrap();
    let env = Arc::new(FaultyEnv::default());
    let db = Db::open(
        dir.path(),
        Options::default()
            .env(Arc::clone(&env) as Arc<dyn Env>)
            .write_buffer_size(8 * 1024)
            .block_size(256)
            .block_cache_size(0)
            .max_background_compactions(0),
    )
    .unwrap();
    for i in 0..400 {
        regolith::through_stalls(|| db.put(&numbered(i), b"in a table")).unwrap();
    }
    db.flush().unwrap();
    let mut queue = db.io_queue();
    // A frozen memtable whose flush fails, so the close's flush fails.
    env.fail_tables(true);
    let mut filler = 0;
    while db
        .get_int_property("regolith.num-entries-imm-mem-tables")
        .unwrap()
        == 0
    {
        regolith::through_stalls(|| {
            db.put(format!("filler/{filler:06}").as_bytes(), &[b'f'; 1024])
        })
        .unwrap();
        filler += 1;
    }
    assert!(db.close().is_err(), "the close's flush fails");
    env.fail_tables(false);

    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    assert_eq!(
        through_queue(&mut queue, || snapshot.get(&numbered(7))).unwrap(),
        Some(b"in a table".to_vec()),
        "a read through the queue after the failed close"
    );
    db.close().unwrap();
}
