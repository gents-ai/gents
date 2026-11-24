//! Concurrent read-write (RW) and read-write-read (RWR) transactions, eight
//! threads each, at every isolation level: the gate the isolation work has
//! to clear before anything builds on the levels.
//!
//! Every scenario forces the interleaving it is about rather than hoping a
//! scheduler produces it. A rendezvous lines the threads up inside their
//! transactions, or a helper thread commits between two reads and
//! acknowledges before the second one, so each invariant is checked
//! against a history that actually contains the hazard. Where a scenario
//! also runs freely, it requires evidence that the free run overlapped.
//! Every wait has a deadline, and a helper ends when the last thread that can
//! ask it for anything is gone.
//!
//! Scenarios 1 to 4, 7, 9 and 11 run on both transaction flavours, and 10 runs
//! its forced half on both; 5, 6 and 8 need the optimistic one. Every scenario
//! runs on two storage shapes: the default,
//! where only the active memtable is ever probed, and a small write buffer
//! with flushes in the worker loops, where the conflict probes walk SSTables
//! too. A scenario claims a probe reached a table only after counting block
//! cache lookups on the committing thread.
//!
//! 1. Lost update. Eight transactions read one counter at the same value,
//!    then all commit `value + 1`. Exactly one may commit at every level and
//!    on both flavours; the rest retry, and the counter ends at exactly the
//!    number of increments.
//! 2. Snapshot sum. An auditor reads part of a set of accounts, a transfer
//!    out of a read account into an unread one commits, and the auditor
//!    reads the rest: its total is still the initial total. Then eight
//!    writers transfer freely until an audit has seen a commit land inside
//!    it, and every audit and the final state hold the total.
//! 3. Repeatable point read. Each of eight readers reads a shared key, a
//!    writer commits a new value and acknowledges, and the reader reads the
//!    key again: the same value, at every level. It reads with `get` and with
//!    `get_for_update`, then writes its own key and reads it back. The commit
//!    aborts exactly at the levels that validate that kind of read.
//! 4. Repeatable scan. Each of eight readers scans a prefix, an inserter
//!    commits a new key into it and acknowledges, and the reader scans
//!    again: the same keys, every seeded key among them, one of them read
//!    first with `get` or `get_for_update`. A transaction begun afterwards
//!    sees the insertion.
//! 5. Blind merges (`DefraLevel`, optimistic). Eight transactions merge into
//!    one hot counter without reading it, and every one has a write forced
//!    onto the counter between its begin and its commit: another operand, a
//!    put, a delete or a range delete. Operands never abort a blind merge at
//!    `DefraLevel`, and a replacement always does; the counter is checked
//!    against a model after every round, with and without a flush or a
//!    compaction in between.
//! 6. Commutative prefix (`DefraLevel`, optimistic). Eight transactions scan
//!    a head prefix and write a key of their own and one fresh key they all
//!    write alike: with a classifier all commit at `DefraLevel`, at any other
//!    level one does, and so does a scan that leaves the prefix.
//! 7. Scan reader commits. A reader scans, a write to a row it was returned
//!    commits, and then the reader commits: only `Serializable` validates a
//!    scan, but a row the reader scanned and then writes is validated at
//!    every level.
//! 8. Content-addressed keys (`DefraLevel`, optimistic). Eight transactions
//!    create one block, each reading whether it is stored before putting it:
//!    with a classifier all commit at `DefraLevel`, anywhere else one does.
//!    A block that was read and found is refused only by a write that takes
//!    it away, a put of one is never validated, and a delete of one is
//!    validated like any delete.
//! 9. Appends. Eight transactions append to one log, with a once key they
//!    share, and none is refused at any level: positions are dense from 1,
//!    follow the commits' receipts, and the once key took one entry. A
//!    commit refused over another key appends nothing, and a transaction
//!    that reads the head and then the entries finds every one while appends
//!    commit around it.
//! 10. Projected reads (`DefraLevel`, optimistic). Eight transactions read a
//!     part of one document and increment that part: the first on each part
//!     commits, where at any other level the first of all does. A read by
//!     parts is refused by a forced write only when it changes a named part.
//! 11. Write-free transactions. A commit that wrote nothing validates no
//!     plain read at `DefraLevel` on the optimistic flavour and is placed at
//!     its snapshot; a read through `get_for_update` is still checked, and
//!     every other level validates what it always does.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem. The browser suite lives in tests/wasm_opfs*.rs.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use regolith::{
    Db, IsolationLevel, OptimisticTransactionDb, Transaction, TransactionDb, TransactionError,
    TxResult, TxnOptions,
};
use tempfile::TempDir;

/// The scenarios that need a database of their own shape live in their own
/// files. The `#[path]` attributes are what keep `tests/concurrent_rw_rwr/`
/// from being picked up as further test targets.
#[path = "concurrent_rw_rwr/appends.rs"]
mod appends;
#[path = "concurrent_rw_rwr/blind_merge.rs"]
mod blind_merge;
#[path = "concurrent_rw_rwr/commutative_prefix.rs"]
mod commutative_prefix;
#[path = "concurrent_rw_rwr/content_addressed.rs"]
mod content_addressed;
/// The merge operator over values of three parts the projected reads use.
#[path = "common/parted.rs"]
mod parted;
#[path = "concurrent_rw_rwr/projected_reads.rs"]
mod projected_reads;
#[path = "concurrent_rw_rwr/scan_commit.rs"]
mod scan_commit;
#[path = "concurrent_rw_rwr/support.rs"]
mod support;
#[path = "concurrent_rw_rwr/write_free.rs"]
mod write_free;

use support::{Rendezvous, STORAGES, Storage, table_lookups};

/// Threads per scenario.
const THREADS: usize = 8;

/// Retry budget per transaction in attempts: bounded so a livelock fails
/// loudly.
const MAX_ATTEMPTS: usize = 2_000;

/// Retry budget per transaction in time, for a machine that makes each
/// attempt slow.
const RETRY_DEADLINE: Duration = Duration::from_secs(30);

/// How long a forced handoff may take before the test fails instead of
/// hanging.
const HANDOFF: Duration = Duration::from_secs(30);

const LEVELS: [IsolationLevel; 5] = [
    IsolationLevel::ReadCommitted,
    IsolationLevel::SnapshotIsolation,
    IsolationLevel::RepeatableRead,
    IsolationLevel::Serializable,
    IsolationLevel::DefraLevel,
];

/// Run `case` on every storage shape at every isolation level, naming the
/// pair when one fails so that a break only `Tables` shows is attributable.
fn each_case(case: impl Fn(Storage, IsolationLevel)) {
    for storage in STORAGES {
        for level in LEVELS {
            if let Err(panic) = catch_unwind(AssertUnwindSafe(|| case(storage, level))) {
                eprintln!("failed on {storage:?} storage at {level:?}");
                resume_unwind(panic);
            }
        }
    }
}

/// Whether a plain point read is validated at commit.
fn validates_point_reads(level: IsolationLevel) -> bool {
    matches!(
        level,
        IsolationLevel::RepeatableRead | IsolationLevel::Serializable | IsolationLevel::DefraLevel
    )
}

fn encode(value: u64) -> [u8; 8] {
    value.to_le_bytes()
}

fn decode(raw: Option<Vec<u8>>) -> u64 {
    raw.map_or(0, |bytes| {
        u64::from_le_bytes(bytes.as_slice().try_into().expect("8-byte value"))
    })
}

fn retryable<T>(result: &TxResult<T>) -> bool {
    matches!(
        result,
        Err(TransactionError::Busy(_) | TransactionError::Conflict { .. })
    )
}

/// Wait after the `refused`-th refusal: yield at first, then sleep for an
/// interval that doubles up to a cap, so contenders spread out without any
/// wait being a fixed interval.
fn back_off(refused: usize) {
    const YIELDS: usize = 4;
    if refused < YIELDS {
        thread::yield_now();
    } else {
        thread::sleep(Duration::from_micros(50 << (refused - YIELDS).min(5)));
    }
}

/// Run `attempt` until it commits, retrying only conflicts and lock
/// timeouts. Returns how many attempts were refused.
///
/// Bounded twice, by attempts and by `RETRY_DEADLINE`, with a growing
/// back-off between attempts, so a livelock fails loudly and a slow machine
/// is not hammered while it waits.
fn commit_with_retry<T>(what: &str, mut attempt: impl FnMut() -> TxResult<T>) -> u64 {
    let deadline = Instant::now() + RETRY_DEADLINE;
    for refused in 0..MAX_ATTEMPTS {
        match attempt() {
            Ok(_) => return refused as u64,
            result if retryable(&result) => back_off(refused),
            Err(e) => panic!("{what}: unexpected transaction error: {e}"),
        }
        assert!(
            Instant::now() < deadline,
            "{what}: did not commit within {RETRY_DEADLINE:?} ({} attempts refused)",
            refused + 1
        );
    }
    panic!("{what}: did not commit within {MAX_ATTEMPTS} attempts");
}

/// Sets `stop` when dropped, so background threads in a scope wind down
/// even when an assertion panics on the scope's main thread.
struct StopOnDrop<'a>(&'a AtomicBool);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Both transaction flavours behind one interface.
trait Flavour: Sync {
    fn begin_at(&self, level: IsolationLevel) -> Transaction;
    fn raw(&self) -> &Db;
}

impl Flavour for OptimisticTransactionDb {
    fn begin_at(&self, level: IsolationLevel) -> Transaction {
        self.begin(&TxnOptions::new().isolation(level))
    }
    fn raw(&self) -> &Db {
        self.db()
    }
}

impl Flavour for TransactionDb {
    fn begin_at(&self, level: IsolationLevel) -> Transaction {
        self.begin(&TxnOptions::new().isolation(level))
    }
    fn raw(&self) -> &Db {
        self.db()
    }
}

fn optimistic(dir: &TempDir, storage: Storage) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir.path(), storage.options()).unwrap()
}

fn pessimistic(dir: &TempDir, storage: Storage) -> TransactionDb {
    TransactionDb::open(dir.path(), storage.options())
        .unwrap()
        .with_lock_timeout(Duration::from_millis(200))
}

// ---- 1. lost update -------------------------------------------------------

/// All readers read the counter at 0 before any of them writes, so every
/// commit after the first carries a stale read of a key it writes.
///
/// On `Tables` the transaction of thread 0 commits alone and the memtable is
/// flushed behind it before the other seven try, and none of them retries
/// until all seven first attempts are in, so each refused commit has to find
/// the winner's version in an SSTable, not in the active memtable.
fn lost_update(db: &impl Flavour, level: IsolationLevel, storage: Storage) {
    db.raw().put(b"counter", &encode(0)).unwrap();
    storage.flush(db.raw());
    let all_read = Rendezvous::new(THREADS);
    let phase = Rendezvous::new(THREADS);
    let first_round_commits = AtomicU64::new(0);

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (all_read, phase, first_round_commits) = (&all_read, &phase, &first_round_commits);
            scope.spawn(move || {
                let tx = db.begin_at(level);
                // Arrive at the rendezvous whatever the read did, so one
                // failed reader cannot strand the other seven; judge it after.
                let read = tx.get(b"counter");
                all_read.wait();
                let seen = decode(read.unwrap());
                assert_eq!(seen, 0, "{level:?}: every reader starts from 0");
                if storage.is_tables() && t != 0 {
                    phase.wait(); // the winner has committed and flushed
                }
                // A pessimistic put can time out on the winner's lock: that
                // is a lost race like a refused commit.
                let (result, lookups) =
                    table_lookups(|| match tx.put(b"counter", &encode(seen + 1)) {
                        Ok(()) => tx.commit(),
                        Err(e) => {
                            drop(tx);
                            Err(e)
                        }
                    });
                if storage.is_tables() {
                    if t == 0 {
                        storage.flush(db.raw());
                        phase.wait();
                    }
                    phase.wait(); // every first attempt is in
                }
                if result.is_ok() {
                    first_round_commits.fetch_add(1, Ordering::Relaxed);
                } else {
                    assert!(retryable(&result), "{level:?}: {result:?}");
                    if storage.is_tables()
                        && matches!(result, Err(TransactionError::Conflict { .. }))
                    {
                        assert!(
                            lookups > 0,
                            "{level:?}: a commit refused behind a flush never probed a table"
                        );
                    }
                    // The stale write lost; increment again from a fresh read.
                    commit_with_retry("counter retry", || {
                        let tx = db.begin_at(level);
                        let value = decode(tx.get(b"counter")?);
                        tx.put(b"counter", &encode(value + 1))?;
                        tx.commit()
                    });
                }
            });
        }
    });

    assert_eq!(
        first_round_commits.load(Ordering::Relaxed),
        1,
        "{level:?}: of {THREADS} transactions that read the same value, exactly one may commit"
    );
    assert_eq!(
        decode(db.raw().get(b"counter").unwrap()),
        THREADS as u64,
        "{level:?}: no increment is lost"
    );
}

#[test]
fn a_stale_read_modify_write_never_commits_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        lost_update(&optimistic(&dir, storage), level, storage);
    });
}

#[test]
fn a_stale_read_modify_write_never_commits_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        lost_update(&pessimistic(&dir, storage), level, storage);
    });
}

// ---- 2. snapshot sum ------------------------------------------------------

const ACCOUNTS: u64 = 8;
const BALANCE: u64 = 100;
const TOTAL: u64 = ACCOUNTS * BALANCE;

fn account(i: u64) -> Vec<u8> {
    format!("account/{i}").into_bytes()
}

fn transfer(db: &impl Flavour, level: IsolationLevel, from: u64, to: u64) -> TxResult<()> {
    let tx = db.begin_at(level);
    let a = decode(tx.get(&account(from))?);
    let b = decode(tx.get(&account(to))?);
    if a == 0 {
        return Ok(());
    }
    tx.put(&account(from), &encode(a - 1))?;
    tx.put(&account(to), &encode(b + 1))?;
    tx.commit().map(|_| ())
}

fn audit(tx: &Transaction) -> u64 {
    (0..ACCOUNTS)
        .map(|i| decode(tx.get(&account(i)).unwrap()))
        .sum()
}

fn snapshot_sum(db: &impl Flavour, level: IsolationLevel, storage: Storage) {
    for i in 0..ACCOUNTS {
        db.raw().put(&account(i), &encode(BALANCE)).unwrap();
    }
    storage.flush(db.raw());

    // Forced: a transfer out of an account the audit already read, into
    // one it has not, commits in the middle of the audit. On `Tables` a
    // flush and a compaction follow it while the audit's snapshot is pinned:
    // the versions the audit has yet to read must survive both.
    let auditor = db.begin_at(level);
    let first = decode(auditor.get(&account(0)).unwrap());
    let (_, lookups) = table_lookups(|| {
        commit_with_retry("forced transfer", || transfer(db, level, 0, ACCOUNTS - 1))
    });
    if storage.is_tables() {
        assert!(
            lookups > 0,
            "{level:?}: the forced transfer never reached a table"
        );
        db.raw().flush().unwrap();
        db.raw().compact_range(None, None).wait().unwrap();
    }
    let rest: u64 = (1..ACCOUNTS)
        .map(|i| decode(auditor.get(&account(i)).unwrap()))
        .sum();
    assert_eq!(
        first + rest,
        TOTAL,
        "{level:?}: an audit reads one snapshot"
    );
    drop(auditor);

    // Free: writers keep transferring until an audit has seen a commit land
    // inside it and, on `Tables`, until a transfer has gone to a table. An
    // audit overlapped when the engine's sequence advanced between just after
    // its transaction began and its last read: a write committed strictly
    // inside it. A transfer that moves nothing writes nothing, so it cannot
    // fake one.
    let overlapped = AtomicBool::new(false);
    let reached_tables = AtomicBool::new(!storage.is_tables());
    let enough = || overlapped.load(Ordering::Acquire) && reached_tables.load(Ordering::Acquire);
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        for t in 0..THREADS as u64 {
            let (enough, reached_tables, stop) = (&enough, &reached_tables, &stop);
            scope.spawn(move || {
                // A writer that panics stops the auditor too.
                let _stop = StopOnDrop(stop);
                let mut n = 0u64;
                while !stop.load(Ordering::Acquire) && (!enough() || n < 50) {
                    let (from, to) = ((t + n) % ACCOUNTS, (t + n + 3) % ACCOUNTS);
                    let (_, lookups) = table_lookups(|| {
                        commit_with_retry("transfer", || transfer(db, level, from, to))
                    });
                    if lookups > 0 {
                        reached_tables.store(true, Ordering::Release);
                    }
                    n += 1;
                    storage.churn(db.raw(), n);
                }
            });
        }
        let deadline = Instant::now() + HANDOFF;
        let mut audits = 0u64;
        while !enough() || audits < 20 {
            assert!(
                !stop.load(Ordering::Acquire) || enough(),
                "{level:?}: the writers stopped before the evidence was in"
            );
            assert!(
                Instant::now() < deadline,
                "{level:?}: within {HANDOFF:?} an audit overlapped a commit: {}, a transfer reached a table: {}",
                overlapped.load(Ordering::Acquire),
                reached_tables.load(Ordering::Acquire)
            );
            let tx = db.begin_at(level);
            let began = db.raw().snapshot().sequence();
            assert_eq!(audit(&tx), TOTAL, "{level:?}: audit {audits}");
            if db.raw().snapshot().sequence() > began {
                overlapped.store(true, Ordering::Release);
            }
            audits += 1;
        }
    });

    let tx = db.begin_at(level);
    assert_eq!(audit(&tx), TOTAL, "{level:?}: final total");
}

#[test]
fn an_audit_reads_one_snapshot_while_transfers_commit_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        snapshot_sum(&optimistic(&dir, storage), level, storage);
    });
}

#[test]
fn an_audit_reads_one_snapshot_while_transfers_commit_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        snapshot_sum(&pessimistic(&dir, storage), level, storage);
    });
}

// ---- 3 and 4. repeatable reads --------------------------------------------

/// Run `reader` on eight threads against one helper thread that commits
/// `write` between a reader's two reads when asked.
///
/// The helper owns every acknowledgement sender and ends when the last
/// request sender is dropped. A reader that finishes or panics, or a helper
/// that panics, therefore releases whoever waits on it at once, with no flag
/// for anyone to poll.
fn with_forced_writes<D: Flavour>(
    db: &D,
    write: impl Fn(&D, usize) + Sync,
    reader: impl Fn(usize, &dyn Fn()) + Sync,
) {
    let (request, requests) = mpsc::channel::<usize>();
    let (acks, ack_rx): (Vec<_>, Vec<_>) = (0..THREADS).map(|_| mpsc::channel::<()>()).unzip();
    thread::scope(|scope| {
        let (write, reader) = (&write, &reader);
        scope.spawn(move || {
            for asker in requests {
                write(db, asker);
                // A send fails only once its reader has gone, and that
                // reader has already failed on its own account.
                let _ = acks[asker].send(());
            }
        });
        let readers: Vec<_> = ack_rx
            .into_iter()
            .enumerate()
            .map(|(r, ack)| {
                let request = request.clone();
                scope.spawn(move || {
                    let force = || {
                        request.send(r).expect("the commit helper is running");
                        ack.recv_timeout(HANDOFF).unwrap_or_else(|e| {
                            panic!("the forced commit was not acknowledged: {e:?}")
                        });
                    };
                    reader(r, &force);
                })
            })
            .collect();
        // Only the readers' clones may keep the helper alive.
        drop(request);
        let outcomes: Vec<_> = readers.into_iter().map(|h| h.join()).collect();
        for outcome in outcomes {
            if let Err(panic) = outcome {
                resume_unwind(panic);
            }
        }
    });
}

fn shared(r: usize) -> Vec<u8> {
    format!("shared/{r}").into_bytes()
}

fn own(r: usize) -> Vec<u8> {
    format!("own/{r}").into_bytes()
}

/// How a reader reads the key a scenario changes under it.
#[derive(Clone, Copy, Debug)]
enum Read {
    /// `get`: validated at commit from `RepeatableRead` up.
    Plain,
    /// `get_for_update`: validated at commit at every level but
    /// `ReadCommitted`, and, for a pessimistic reader, a lock it holds.
    ForUpdate,
}

const READS: [Read; 2] = [Read::Plain, Read::ForUpdate];

impl Read {
    fn of(self, tx: &Transaction, key: &[u8]) -> Option<Vec<u8>> {
        match self {
            Self::Plain => tx.get(key),
            Self::ForUpdate => tx.get_for_update(key),
        }
        .unwrap()
    }

    /// Whether the commit validates a key read this way and never written.
    fn validated_at(self, level: IsolationLevel) -> bool {
        match self {
            Self::Plain => validates_point_reads(level),
            Self::ForUpdate => level != IsolationLevel::ReadCommitted,
        }
    }
}

fn point_read_repeats<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage, read: Read) {
    for r in 0..THREADS {
        db.raw().put(&shared(r), &encode(1)).unwrap();
    }
    storage.flush(db.raw());
    with_forced_writes(
        db,
        |db, r| {
            let value = decode(db.raw().get(&shared(r)).unwrap());
            db.raw().put(&shared(r), &encode(value + 1)).unwrap();
            // The commit that follows must find this version in a table.
            storage.flush(db.raw());
        },
        |r, force| {
            let tx = db.begin_at(level);
            let first = decode(read.of(&tx, &shared(r)));
            force();
            assert_eq!(
                decode(db.raw().get(&shared(r)).unwrap()),
                first + 1,
                "the forced commit is visible outside the transaction"
            );
            let second = decode(read.of(&tx, &shared(r)));
            assert_eq!(
                first, second,
                "{level:?} {read:?}: reader {r} read one snapshot"
            );
            tx.put(&own(r), &encode(first)).unwrap();
            assert_eq!(decode(tx.get(&own(r)).unwrap()), first, "read your writes");
            let (result, lookups) = table_lookups(|| tx.commit());
            if read.validated_at(level) {
                assert!(
                    matches!(result, Err(TransactionError::Conflict { .. })),
                    "{level:?} {read:?}: a validated read overtaken by a commit aborts: {result:?}"
                );
                assert!(
                    !storage.is_tables() || lookups > 0,
                    "{level:?} {read:?}: the refused commit never probed the table"
                );
            } else {
                assert!(
                    result.is_ok(),
                    "{level:?} {read:?}: an unvalidated read commits: {result:?}"
                );
            }
        },
    );
}

#[test]
fn a_point_read_repeats_while_its_key_changes_optimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            point_read_repeats(&optimistic(&dir, storage), level, storage, read);
        }
    });
}

#[test]
fn a_point_read_repeats_while_its_key_changes_pessimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            point_read_repeats(&pessimistic(&dir, storage), level, storage, read);
        }
    });
}

const SEEDED: usize = 4;

/// The row reader `r` reads before its first scan.
fn mine(r: usize) -> Vec<u8> {
    format!("row/mine-{r}").into_bytes()
}

/// The row reader `r` reads after the forced insertion and before its second
/// scan.
fn late(r: usize) -> Vec<u8> {
    format!("row/late-{r}").into_bytes()
}

fn scanned(tx: &Transaction) -> BTreeSet<Vec<u8>> {
    tx.scan_stream(Some(b"row/"), Some(b"row0"))
        .map(|item| item.unwrap().0)
        .collect()
}

fn scan_repeats<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage, read: Read) {
    let seeded: BTreeSet<Vec<u8>> = (0..SEEDED)
        .map(|i| format!("row/seed-{i}").into_bytes())
        .chain((0..THREADS).map(mine))
        .chain((0..THREADS).map(late))
        .collect();
    for key in &seeded {
        db.raw().put(key, b"x").unwrap();
    }
    storage.flush(db.raw());
    with_forced_writes(
        db,
        |db, r| {
            db.raw()
                .put(format!("row/new-{r}").as_bytes(), b"x")
                .unwrap();
            storage.flush(db.raw());
        },
        |r, force| {
            let tx = db.begin_at(level);
            // A row read before the scan: a pessimistic `get_for_update`
            // locks it, and the scan has to serve it where `get` does, in the
            // middle of a stretch.
            assert_eq!(
                read.of(&tx, &mine(r)).as_deref(),
                Some(&b"x"[..]),
                "{level:?} {read:?}: reader {r} reads its first row"
            );
            let first = scanned(&tx);
            assert!(
                first.is_superset(&seeded),
                "{level:?} {read:?}: reader {r} sees every seeded row"
            );
            force();
            // A lock taken after the forced commit is sampled past the begin
            // snapshot, so the second scan serves this row at a sequence of its
            // own: the row is the same, and the insertion stays out of sight.
            assert_eq!(
                read.of(&tx, &late(r)).as_deref(),
                Some(&b"x"[..]),
                "{level:?} {read:?}: reader {r} reads its second row"
            );
            let (second, lookups) = table_lookups(|| scanned(&tx));
            assert!(
                !storage.is_tables() || lookups > 0,
                "{level:?} {read:?}: reader {r}'s second scan never reached a table"
            );
            assert_eq!(
                first, second,
                "{level:?} {read:?}: reader {r} scanned one snapshot"
            );
            let inserted = format!("row/new-{r}").into_bytes();
            assert!(!second.contains(&inserted));
            assert!(
                scanned(&db.begin_at(level)).contains(&inserted),
                "{level:?} {read:?}: a later transaction sees the insertion"
            );
        },
    );
}

#[test]
fn a_scan_repeats_while_its_range_grows_optimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            scan_repeats(&optimistic(&dir, storage), level, storage, read);
        }
    });
}

#[test]
fn a_scan_repeats_while_its_range_grows_pessimistic() {
    each_case(|storage, level| {
        for read in READS {
            let dir = tempfile::tempdir().unwrap();
            scan_repeats(&pessimistic(&dir, storage), level, storage, read);
        }
    });
}
