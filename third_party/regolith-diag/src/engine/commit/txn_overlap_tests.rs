//! Optimistic transactions decided together in the commit pipeline (E10,
//! `CommitPipeline.tla`), through the public API, with the first overlap
//! made certain instead of left to the scheduler.
//!
//! Many threads run read-modify-write transactions over a few hot keys at
//! Immediate durability. The history is then checked against a run of the
//! committed transactions one at a time in receipt order: every value a
//! committed transaction read through `get_for_update` is the value that run
//! holds at its turn, and the database ends where that run ends. A slot
//! decided without the writes of the commits before it would show a read the
//! serial run contradicts (RED PassBeforeApply), and a lost update would
//! show in the final counters.
//!
//! Threads started together overlap only by luck: on one or two cores, or on
//! a filesystem whose fsync returns at once, each commit can finish before
//! the next begins. So the round, the one claim on the log's device step, is
//! held, as a thread in the middle of a sync holds it, until every thread's
//! first commit is decided: each is then in the ring, validated against the
//! ones before it, and waits behind the round. When the round is let go, one
//! sync covers them all, on any core count and any filesystem.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;

use super::overlap_tests::wait_until_decided;
use crate::statistics::Ticker;
use crate::{
    IsolationLevel, OptimisticTransactionDb, Options, Statistics, TransactionError, TxnOptions,
};

const KEYS: usize = 6;
const THREADS: usize = 12;
const COMMITS_PER_THREAD: usize = 40;
const MAX_ATTEMPTS: usize = 10_000;

fn key(i: usize) -> Vec<u8> {
    format!("counter/{i}").into_bytes()
}

fn number(bytes: Option<Vec<u8>>) -> u64 {
    bytes.map_or(0, |b| String::from_utf8(b).unwrap().parse().unwrap())
}

/// One committed transaction: its receipt sequence, what it read, what it wrote.
struct Committed {
    seq: u64,
    reads: Vec<(usize, u64)>,
    writes: Vec<(usize, u64)>,
}

/// A small deterministic generator per thread.
struct Rng(u64);

impl Rng {
    fn of_thread(t: usize) -> Self {
        Rng(0xC0FF_EE00 ^ t as u64)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// The two keys a thread's next transaction reads; it writes the first.
    fn pair(&mut self) -> (usize, usize) {
        let a = self.next() as usize % KEYS;
        let b = (a + 1 + self.next() as usize % (KEYS - 1)) % KEYS;
        (a, b)
    }
}

/// Whichever thread's commit is decided first, some other thread's first
/// transaction reads neither key the first one writes, so at least two of
/// the first commits succeed and wait for the same sync.
fn assert_two_first_commits_can_share() {
    let pairs: Vec<(usize, usize)> = (0..THREADS).map(|t| Rng::of_thread(t).pair()).collect();
    for (first, &(written, _)) in pairs.iter().enumerate() {
        assert!(
            pairs
                .iter()
                .enumerate()
                .any(|(other, &(a, b))| other != first && a != written && b != written),
            "setup: every first transaction reads key {written}, which thread {first} writes"
        );
    }
}

/// Run the workload, check its history against the one-at-a-time run, and
/// return how many fsyncs its commits took.
fn run_once(isolation: IsolationLevel) -> u64 {
    assert_two_first_commits_can_share();
    let dir = TempDir::new().unwrap();
    let stats = Arc::new(Statistics::new());
    let db = Arc::new(
        OptimisticTransactionDb::open(
            dir.path(),
            Options::default()
                .durability(crate::DurabilityMode::Immediate)
                .statistics(Some(Arc::clone(&stats))),
        )
        .unwrap(),
    );
    let engine = Arc::clone(&db.db().engine);
    let pipe = engine.pipe().unwrap();
    let before = pipe.ring.decided();

    // Held as a thread holds it while it syncs: a commit that arrives is
    // decided, and then waits for the round, as does a transaction that
    // lost a conflict, whose answer comes in ring order.
    let held = engine.hold_round(pipe, super::park::Help::Wait);
    let handles: Vec<_> = (0..THREADS)
        .map(|t| {
            let db = Arc::clone(&db);
            thread::spawn(move || {
                let mut rng = Rng::of_thread(t);
                let mut committed = Vec::with_capacity(COMMITS_PER_THREAD);
                for _ in 0..COMMITS_PER_THREAD {
                    let (a, b) = rng.pair();
                    let mut attempts = 0;
                    loop {
                        attempts += 1;
                        assert!(attempts < MAX_ATTEMPTS, "thread {t} never committed");
                        let tx = db.begin(&TxnOptions::new().isolation(isolation));
                        let va = number(tx.get_for_update(&key(a)).unwrap());
                        let vb = number(tx.get_for_update(&key(b)).unwrap());
                        // `a` takes a value derived from both reads that never
                        // repeats one of them, so every commit depends on both.
                        let next =
                            (va.wrapping_mul(31).wrapping_add(vb).wrapping_add(1)) % 1_000_000_007;
                        tx.put(&key(a), next.to_string().as_bytes()).unwrap();
                        match tx.commit() {
                            Ok(receipt) => {
                                committed.push(Committed {
                                    seq: receipt.seq(),
                                    reads: vec![(a, va), (b, vb)],
                                    writes: vec![(a, next)],
                                });
                                break;
                            }
                            Err(TransactionError::Conflict(_)) => continue,
                            Err(other) => panic!("thread {t}: {other}"),
                        }
                    }
                }
                committed
            })
        })
        .collect();
    wait_until_decided(pipe, before + THREADS as u64);
    assert_eq!(
        stats.get_ticker(Ticker::CommitCount),
        0,
        "no commit returns while the round is held"
    );
    drop(held);

    let mut history: Vec<Committed> = handles
        .into_iter()
        .flat_map(|h| h.join().expect("a writer panicked"))
        .collect();
    history.sort_by_key(|c| c.seq);
    assert!(
        history.windows(2).all(|w| w[0].seq < w[1].seq),
        "every commit that wrote took a sequence of its own"
    );

    let mut state: BTreeMap<usize, u64> = (0..KEYS).map(|k| (k, 0)).collect();
    for (turn, commit) in history.iter().enumerate() {
        for &(k, v) in &commit.reads {
            assert_eq!(
                state[&k], v,
                "commit {turn} (seq {}) read key {k} as {v}; committing one at a time \
                 in receipt order it holds {}",
                commit.seq, state[&k]
            );
        }
        for &(k, v) in &commit.writes {
            state.insert(k, v);
        }
    }
    for (k, v) in &state {
        assert_eq!(number(db.db().get(&key(*k)).unwrap()), *v, "key {k}");
    }

    assert_eq!(
        stats.get_ticker(Ticker::CommitCount),
        (THREADS * COMMITS_PER_THREAD) as u64
    );
    stats.get_ticker(Ticker::WalSyncCount)
}

/// The run is checked against the serial run, and its first commits,
/// decided behind the held round, share one fsync.
fn run(isolation: IsolationLevel) {
    let commits = (THREADS * COMMITS_PER_THREAD) as u64;
    let fsyncs = run_once(isolation);
    assert!(
        fsyncs < commits,
        "{commits} Immediate commits by {THREADS} threads took {fsyncs} fsyncs: \
         no commit ever shared an fsync"
    );
}

#[test]
fn concurrent_transactions_decided_together_equal_a_serial_run_in_commit_order() {
    run(IsolationLevel::SnapshotIsolation);
}

#[test]
fn serializable_transactions_decided_together_equal_a_serial_run_in_commit_order() {
    run(IsolationLevel::Serializable);
}
