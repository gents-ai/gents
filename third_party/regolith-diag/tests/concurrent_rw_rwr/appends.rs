//! Scenario 9: commit-ordered appends, at every level and on both flavours.
//!
//! One log. The optimistic database declares its keys a log, so at
//! `DefraLevel` the classifier is consulted for every append.
//!
//! Overlap. Eight transactions each append an entry of their own, an entry
//! under a once key the eight share, and a second entry of their own, and
//! only when all eight have buffered theirs does the first commit. An append
//! is never validated, so at every level and on both flavours none of the
//! eight commits is refused. After each round the log is judged whole:
//! positions run from 1 to the head with no hole, a transaction's own two
//! entries sit next to each other in the order it appended them, exactly one
//! of the eight entries offered under the once key is there and the once key
//! holds its position, and the positions follow the order of the commits'
//! receipts. On `Tables` the head is flushed between rounds, so a round
//! numbers its entries from a head that lives in a table.
//!
//! Refused. Each of eight transactions reads a counter of its own, appends,
//! and writes the counter back one higher after a write was forced onto it:
//! a stale read-modify-write, refused at every level. A refused commit
//! applies nothing, so its entry is not in the log and left no hole; the
//! retry's entry is, once.
//!
//! Dense prefix. Eight appenders run freely while audits each read the head
//! in a transaction, wait for another append to commit outside it, and then
//! read every entry up to that head and the position after it: every entry is
//! there, nothing is beyond the head, and the head has not moved.
//!
//! Threads in the overlap never assert between two rendezvous, since a thread
//! that panicked would strand the rest; they record what they saw and the
//! test judges after the scope has ended.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use regolith::{
    Db, IsolationLevel, KeyClass, KeyClassifier, LogLayout, OptimisticTransactionDb,
    TransactionError,
};

use super::support::{Findings, Rendezvous, Storage, table_lookups};
use super::{
    Flavour, HANDOFF, StopOnDrop, THREADS, back_off, commit_with_retry, decode, each_case, encode,
    pessimistic, with_forced_writes,
};

const ROUNDS: usize = 3;

const HEAD: &[u8] = b"journal-head";

/// Entries at `journal/<20 decimal digits>`, the head at `journal-head`.
struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        HEAD
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(entry_key(position).as_slice());
    }

    fn max_entry_key_len(&self) -> usize {
        "journal/".len() + 20
    }
}

fn journal() -> Arc<dyn LogLayout> {
    Arc::new(Journal)
}

fn entry_key(position: u64) -> Vec<u8> {
    format!("journal/{position:020}").into_bytes()
}

/// The journal and the once keys are log keys.
struct LogKeys;

impl KeyClassifier for LogKeys {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"journal") || key.starts_with(b"once/") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

fn classified(dir: &tempfile::TempDir, storage: Storage) -> OptimisticTransactionDb {
    OptimisticTransactionDb::open(dir.path(), storage.options())
        .unwrap()
        .with_policy(Arc::new(LogKeys))
}

fn position(raw: Option<Vec<u8>>) -> u64 {
    raw.map_or(0, |bytes| {
        u64::from_be_bytes(bytes.as_slice().try_into().expect("an 8-byte position"))
    })
}

/// The log as the database holds it now: the entry at each position from 1 to
/// the head, `None` where one is missing.
fn log_of(db: &Db) -> Vec<Option<Vec<u8>>> {
    (1..=position(db.get(HEAD).unwrap()))
        .map(|p| db.get(&entry_key(p)).unwrap())
        .collect()
}

// ---- overlap --------------------------------------------------------------

/// The `i`-th entry thread `t` appends on its own account in `round`.
fn own_entry(round: usize, t: usize, i: usize) -> Vec<u8> {
    format!("own {round} {t} {i}").into_bytes()
}

/// The entry thread `t` offers under the once key of `round`.
fn once_entry(round: usize, t: usize) -> Vec<u8> {
    format!("once {round} {t}").into_bytes()
}

fn once_key(round: usize) -> Vec<u8> {
    format!("once/{round}").into_bytes()
}

/// Judge the log after `round`, given the sequence each thread's commit
/// reported.
fn judge_round(case: &str, db: &Db, round: usize, receipts: &[Option<u64>], findings: &Findings) {
    let log = log_of(db);
    let expected = (round + 1) * (2 * THREADS + 1);
    findings.check(log.len() == expected, || {
        format!(
            "{case} round {round}: the head is {}, expected {expected}",
            log.len()
        )
    });
    findings.check(log.iter().all(Option::is_some), || {
        format!("{case} round {round}: a position below the head holds no entry")
    });
    // Where each entry sits, positions from 1.
    let at: BTreeMap<&[u8], u64> = log
        .iter()
        .enumerate()
        .filter_map(|(i, entry)| Some((entry.as_deref()?, i as u64 + 1)))
        .collect();
    findings.check(at.len() == log.len(), || {
        format!("{case} round {round}: an entry is in the log twice")
    });

    let offered: Vec<(usize, u64)> = (0..THREADS)
        .filter_map(|t| Some((t, *at.get(once_entry(round, t).as_slice())?)))
        .collect();
    findings.check(offered.len() == 1, || {
        format!("{case} round {round}: the once key took the entries of {offered:?}")
    });
    let once = position(db.get(&once_key(round)).unwrap());
    findings.check(offered.first().is_some_and(|&(_, p)| p == once), || {
        format!("{case} round {round}: the once key holds {once}, its entry is {offered:?}")
    });

    let mut firsts = Vec::new();
    for (t, receipt) in receipts.iter().enumerate() {
        let first = at.get(own_entry(round, t, 0).as_slice()).copied();
        let second = at.get(own_entry(round, t, 1).as_slice()).copied();
        let (Some(first), Some(second)) = (first, second) else {
            findings.check(false, || {
                format!("{case} round {round}: an entry of thread {t} is missing")
            });
            continue;
        };
        // Its once entry sits between the two when it is the one that was
        // taken, and nowhere when it was not.
        let between = u64::from(offered.first().is_some_and(|&(winner, _)| winner == t));
        findings.check(second == first + 1 + between, || {
            format!(
                "{case} round {round}: thread {t} appended at {first} and {second}, \
                 with {between} of its entries between"
            )
        });
        firsts.push((*receipt, first));
    }
    findings.check(firsts.iter().all(|(seq, _)| seq.is_some()), || {
        format!("{case} round {round}: a commit reported no receipt")
    });
    firsts.sort_unstable();
    findings.check(firsts.windows(2).all(|pair| pair[0].0 < pair[1].0), || {
        format!("{case} round {round}: two commits reported one receipt: {firsts:?}")
    });
    findings.check(firsts.windows(2).all(|pair| pair[0].1 < pair[1].1), || {
        format!("{case} round {round}: positions do not follow commit order: {firsts:?}")
    });
}

fn appends_overlap(db: &impl Flavour, level: IsolationLevel, storage: Storage, flavour: &str) {
    let case = format!("{storage:?} {level:?} {flavour}");
    let log = journal();
    let sync = Rendezvous::new(THREADS);
    let findings = Findings::default();
    let receipts: Mutex<Vec<Option<u64>>> = Mutex::new(vec![None; THREADS]);
    let table_probes = AtomicU64::new(0);

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (case, log, sync) = (&case, &log, &sync);
            let (findings, receipts, table_probes) = (&findings, &receipts, &table_probes);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    sync.wait(); // the previous round is judged
                    let tx = db.begin_at(level);
                    let buffered = tx
                        .append(log, &own_entry(round, t, 0), None)
                        .and_then(|()| {
                            tx.append(log, &once_entry(round, t), Some(&once_key(round)))
                        })
                        .and_then(|()| tx.append(log, &own_entry(round, t, 1), None));
                    findings.check(buffered.is_ok(), || {
                        format!("{case} round {round} thread {t}: {buffered:?}")
                    });
                    sync.wait(); // all eight have appended

                    let (committed, lookups) = table_lookups(|| tx.commit());
                    table_probes.fetch_add(lookups, Ordering::Relaxed);
                    let seq = committed.as_ref().ok().map(|receipt| receipt.seq());
                    findings.check(seq.is_some(), || {
                        format!(
                            "{case} round {round} thread {t}: an append was refused: {committed:?}"
                        )
                    });
                    receipts.lock().unwrap()[t] = seq;
                    sync.wait(); // all eight commits are in

                    if t == 0 {
                        let seen =
                            std::mem::replace(&mut *receipts.lock().unwrap(), vec![None; THREADS]);
                        judge_round(case, db.raw(), round, &seen, findings);
                        // The head is read where the commit is ordered. From
                        // the second round on it was flushed, so some commit
                        // of the round read it from a table.
                        let probes = table_probes.swap(0, Ordering::Relaxed);
                        findings.check(!storage.is_tables() || round == 0 || probes > 0, || {
                            format!("{case} round {round}: no commit read the head from a table")
                        });
                        storage.flush(db.raw());
                    }
                }
            });
        }
    });
    findings.judge();
}

#[test]
fn appends_never_conflict_and_number_in_commit_order_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        appends_overlap(&classified(&dir, storage), level, storage, "optimistic");
    });
}

#[test]
fn appends_never_conflict_and_number_in_commit_order_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        appends_overlap(&pessimistic(&dir, storage), level, storage, "pessimistic");
    });
}

// ---- refused --------------------------------------------------------------

fn counter(r: usize) -> Vec<u8> {
    format!("counter/{r}").into_bytes()
}

/// The entry reader `r` appends on its `attempt`-th try.
fn attempt_entry(r: usize, attempt: usize) -> Vec<u8> {
    format!("attempt {r} {attempt}").into_bytes()
}

fn refused_commits_append_nothing<D: Flavour>(db: &D, level: IsolationLevel, storage: Storage) {
    let log = journal();
    for r in 0..THREADS {
        db.raw().put(&counter(r), &encode(1)).unwrap();
    }
    storage.flush(db.raw());
    with_forced_writes(
        db,
        |db, r| {
            let value = decode(db.raw().get(&counter(r)).unwrap());
            db.raw().put(&counter(r), &encode(value + 1)).unwrap();
            // The commit that follows must find this version in a table.
            storage.flush(db.raw());
        },
        |r, force| {
            let tx = db.begin_at(level);
            let seen = decode(tx.get(&counter(r)).unwrap());
            tx.append(&log, &attempt_entry(r, 0), None).unwrap();
            force();
            // A pessimistic put may wait on nothing here: the forced write
            // went around the lock manager.
            tx.put(&counter(r), &encode(seen + 1)).unwrap();
            let (result, lookups) = table_lookups(|| tx.commit());
            match result {
                Err(TransactionError::Conflict(conflict)) => assert_eq!(
                    conflict.key(),
                    counter(r),
                    "{level:?}: reader {r} conflicts on its counter"
                ),
                other => panic!("{level:?}: a stale read-modify-write must abort: {other:?}"),
            }
            assert!(
                !storage.is_tables() || lookups > 0,
                "{level:?}: the refused commit never probed the table"
            );
            commit_with_retry("append beside an increment", || {
                let tx = db.begin_at(level);
                let value = decode(tx.get(&counter(r))?);
                tx.append(&log, &attempt_entry(r, 1), None)?;
                tx.put(&counter(r), &encode(value + 1))?;
                tx.commit()
            });
        },
    );

    let entries = log_of(db.raw());
    assert!(
        entries.iter().all(Option::is_some),
        "{level:?}: a refused commit left a hole in the log"
    );
    let mut entries: Vec<Vec<u8>> = entries.into_iter().flatten().collect();
    entries.sort_unstable();
    let mut retries: Vec<Vec<u8>> = (0..THREADS).map(|r| attempt_entry(r, 1)).collect();
    retries.sort_unstable();
    assert_eq!(
        entries, retries,
        "{level:?}: the log holds each retry's entry once and no refused attempt's"
    );
    for r in 0..THREADS {
        assert_eq!(
            decode(db.raw().get(&counter(r)).unwrap()),
            3,
            "{level:?}: the forced write and the retry, and nothing of the refused attempt"
        );
    }
}

#[test]
fn a_refused_commit_appends_nothing_and_leaves_no_hole_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        refused_commits_append_nothing(&classified(&dir, storage), level, storage);
    });
}

#[test]
fn a_refused_commit_appends_nothing_and_leaves_no_hole_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        refused_commits_append_nothing(&pessimistic(&dir, storage), level, storage);
    });
}

// ---- dense prefix ---------------------------------------------------------

/// Audits of one run, at least.
const AUDITS: u64 = 20;

/// Appends each audit lets the appenders make once it has read the head, so
/// that they land while it reads on and the log stays short enough for every
/// audit to read it whole.
const BURST: u64 = 32;

/// Append `entry` in a transaction of its own and return the block lookups
/// its commit made. Nothing refuses an append, so a refusal fails the test.
fn append_alone(
    db: &impl Flavour,
    level: IsolationLevel,
    log: &Arc<dyn LogLayout>,
    entry: &[u8],
) -> u64 {
    let tx = db.begin_at(level);
    tx.append(log, entry, None).unwrap();
    let (result, lookups) = table_lookups(|| tx.commit());
    if let Err(e) = result {
        panic!("{level:?}: an append was refused: {e}");
    }
    lookups
}

/// One audit: a transaction reads the head, `release` lets the appenders go
/// on, at least one more append commits outside the transaction, and it reads
/// every entry up to the head it read, the position after it and the head
/// again. One snapshot holds the entries 1 to its head and nothing beyond, so
/// each is there, the next position is not, and the head has not moved.
/// Panics otherwise.
fn audit(db: &impl Flavour, level: IsolationLevel, release: impl FnOnce()) {
    let tx = db.begin_at(level);
    let head = position(tx.get(HEAD).unwrap());
    release();
    // Forced: the log outside the transaction grows past the head it read
    // before it reads any further.
    let deadline = Instant::now() + HANDOFF;
    let mut waited = 0;
    while position(db.raw().get(HEAD).unwrap()) <= head {
        assert!(
            Instant::now() < deadline,
            "{level:?}: no append committed within {HANDOFF:?} of an audit's first read"
        );
        back_off(waited);
        waited += 1;
    }
    for p in 1..=head {
        assert!(
            tx.get(&entry_key(p)).unwrap().is_some(),
            "{level:?}: position {p} is missing below the head {head} of one snapshot"
        );
    }
    assert!(
        tx.get(&entry_key(head + 1)).unwrap().is_none(),
        "{level:?}: an entry past the head {head} is in the snapshot the head was read in"
    );
    assert_eq!(
        position(tx.get(HEAD).unwrap()),
        head,
        "{level:?}: the head moved inside one snapshot"
    );
}

fn a_snapshot_sees_a_dense_prefix(db: &impl Flavour, level: IsolationLevel, storage: Storage) {
    let log = journal();
    let reached_tables = AtomicBool::new(!storage.is_tables());
    let appended = AtomicU64::new(0);
    // How many appends the audits have let through so far.
    let allowed = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let _stop = StopOnDrop(&stop);
        for t in 0..THREADS {
            let (log, reached_tables, appended, stop) = (&log, &reached_tables, &appended, &stop);
            let allowed = &allowed;
            scope.spawn(move || {
                // An appender that panics stops the audits too.
                let _stop = StopOnDrop(stop);
                let (mut n, mut idle) = (0u64, 0);
                while !stop.load(Ordering::Acquire) {
                    if appended.load(Ordering::Acquire) >= allowed.load(Ordering::Acquire) {
                        back_off(idle);
                        idle += 1;
                        continue;
                    }
                    idle = 0;
                    let entry = format!("free {t} {n}");
                    let lookups = append_alone(db, level, log, entry.as_bytes());
                    appended.fetch_add(1, Ordering::AcqRel);
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
        while audits < AUDITS || !reached_tables.load(Ordering::Acquire) {
            assert!(
                !stop.load(Ordering::Acquire),
                "{level:?}: an appender stopped before the audits were done"
            );
            assert!(
                Instant::now() < deadline,
                "{level:?}: within {HANDOFF:?} no append read the head from a table"
            );
            audit(db, level, || {
                allowed.fetch_add(BURST, Ordering::AcqRel);
            });
            audits += 1;
        }
    });

    let log = log_of(db.raw());
    assert!(
        log.iter().all(Option::is_some),
        "{level:?}: the final log has a hole"
    );
    let entries: BTreeSet<&[u8]> = log.iter().flatten().map(Vec::as_slice).collect();
    assert_eq!(
        (log.len() as u64, entries.len() as u64),
        (appended.load(Ordering::Relaxed), log.len() as u64),
        "{level:?}: every append that committed is in the log once"
    );
}

#[test]
fn a_transaction_reads_a_dense_prefix_while_appends_commit_optimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        a_snapshot_sees_a_dense_prefix(&classified(&dir, storage), level, storage);
    });
}

#[test]
fn a_transaction_reads_a_dense_prefix_while_appends_commit_pessimistic() {
    each_case(|storage, level| {
        let dir = tempfile::tempdir().unwrap();
        a_snapshot_sees_a_dense_prefix(&pessimistic(&dir, storage), level, storage);
    });
}
