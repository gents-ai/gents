//! Scenario 6: a scan inside a commutative prefix under `DefraLevel`, on the
//! optimistic flavour (the pessimistic one takes no key classifier).
//!
//! The database declares the keys under `h/` a commutative prefix. Eight
//! transactions each begin, scan the heads, write one key of their own and
//! one key that is fresh to the round and that all eight write with the same
//! bytes, and only when all eight have done so does the first of them commit.
//! The commits then overlap by construction.
//!
//! With the classifier, at `DefraLevel`, a scan that stays inside the prefix
//! records no stretch, so the fresh key is a blind write of bytes another
//! commit already stored and all eight commit. Anywhere else the stretch is
//! recorded, the fresh key lies inside it and is validated as read from the
//! begin snapshot, and only the first commit survives: that is every other
//! level, and `DefraLevel` too when the scan walks on to a key outside the
//! prefix. The refused seven retry until they commit, and the keys of every
//! round must all be there afterwards.
//!
//! On `Tables` the first commit is made alone and flushed behind, so the
//! seven others are judged against a version that lives in a table.
//!
//! Threads here never assert between two rendezvous, since a thread that
//! panicked would strand the rest; they record what they saw and the test
//! judges after the scope has ended.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use regolith::{
    IsolationLevel, KeyClass, KeyClassifier, OptimisticTransactionDb, Transaction,
    TransactionError, TxnOptions,
};

use super::support::{Findings, Rendezvous, STORAGES, Storage, table_lookups};
use super::{LEVELS, THREADS, commit_with_retry};

const ROUNDS: usize = 3;

/// The bytes every thread writes under the key fresh to its round.
const SHARED_VALUE: &[u8] = b"superseded";

/// Keys under `h/` are added under fresh names and never contended.
struct Heads;

impl KeyClassifier for Heads {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"h/") {
            KeyClass::CommutativePrefix { len: 2 }
        } else {
            KeyClass::Ordinary
        }
    }
}

/// How far the scan reaches.
#[derive(Clone, Copy, Debug)]
enum Walk {
    /// To the end of the prefix: every key it yields is a head.
    Inside,
    /// Past it, onto a key outside: the stretch it records leaves the prefix.
    Leaving,
}

const WALKS: [Walk; 2] = [Walk::Inside, Walk::Leaving];

impl Walk {
    fn end(self) -> &'static [u8] {
        match self {
            Self::Inside => b"h0",
            Self::Leaving => b"j",
        }
    }
}

/// The key thread `t` writes alone in `round`.
fn unique(round: usize, t: usize) -> Vec<u8> {
    format!("h/u{round}-{t}").into_bytes()
}

/// The key every thread writes alike in `round`.
fn fresh(round: usize) -> Vec<u8> {
    format!("h/m{round}").into_bytes()
}

/// What a scan at the start of `round` yields: the seeds, then the keys of
/// every round before it.
fn heads_before(round: usize, walk: Walk) -> BTreeSet<Vec<u8>> {
    let mut keys = BTreeSet::from([b"h/a".to_vec(), b"h/z".to_vec()]);
    if matches!(walk, Walk::Leaving) {
        keys.insert(b"i/other".to_vec());
    }
    for earlier in 0..round {
        keys.insert(fresh(earlier));
        keys.extend((0..THREADS).map(|t| unique(earlier, t)));
    }
    keys
}

/// Begin, scan the heads and write thread `t`'s two keys. The keys sort
/// between the first and the last head, so they lie inside the stretch the
/// scan walked.
fn stage(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    walk: Walk,
    round: usize,
    t: usize,
) -> (Transaction, BTreeSet<Vec<u8>>) {
    let tx = db.begin(&TxnOptions::new().isolation(level));
    let seen = tx
        .scan_stream(Some(b"h/"), Some(walk.end()))
        .map(|item| item.unwrap().0)
        .collect();
    tx.put(&unique(round, t), &[t as u8]).unwrap();
    tx.put(&fresh(round), SHARED_VALUE).unwrap();
    (tx, seen)
}

fn heads_overlap(level: IsolationLevel, walk: Walk, storage: Storage) {
    let case = format!("{storage:?} {level:?} {walk:?}");
    let dir = tempfile::tempdir().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), storage.options())
        .unwrap()
        .with_policy(Arc::new(Heads));
    for key in heads_before(0, Walk::Leaving) {
        db.db().put(&key, b"cid").unwrap();
    }
    storage.flush(db.db());

    let sync = Rendezvous::new(THREADS);
    let findings = Findings::default();
    let first_commits = AtomicU64::new(0);
    let table_probes = AtomicU64::new(0);
    let all_commit = level == IsolationLevel::DefraLevel && matches!(walk, Walk::Inside);
    let expected = if all_commit { THREADS as u64 } else { 1 };

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (case, db, sync) = (&case, &db, &sync);
            let (findings, first_commits, table_probes) =
                (&findings, &first_commits, &table_probes);
            scope.spawn(move || {
                for round in 0..ROUNDS {
                    sync.wait(); // the previous round is settled
                    let (tx, seen) = stage(db, level, walk, round, t);
                    findings.check(seen == heads_before(round, walk), || {
                        format!("{case} round {round} thread {t}: scanned {seen:?}")
                    });
                    sync.wait(); // all eight have scanned and written

                    if storage.is_tables() && t != 0 {
                        sync.wait(); // thread 0 has committed and flushed
                    }
                    let (first, lookups) = table_lookups(|| tx.commit());
                    if storage.is_tables() {
                        if t == 0 {
                            storage.flush(db.db());
                            sync.wait();
                        } else {
                            table_probes.fetch_add(lookups, Ordering::Relaxed);
                        }
                        // No retry may commit before every first attempt is
                        // in, or a probe would meet its write in the memtable.
                        sync.wait();
                    }
                    match first {
                        Ok(_) => {
                            first_commits.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(TransactionError::Conflict(conflict)) => {
                            let key = conflict.key();
                            findings.check(key == fresh(round), || {
                                format!("{case} round {round}: conflict on {key:?}")
                            });
                            commit_with_retry("heads", || {
                                let (tx, _) = stage(db, level, walk, round, t);
                                tx.commit()
                            });
                        }
                        Err(other) => panic!("{case} round {round}: {other}"),
                    }
                    sync.wait(); // all eight are in

                    if t == 0 {
                        // The first of the seven to commit meets the fresh key in the
                        // table; one that commits after it meets the rewrite in the memtable.
                        let probes = table_probes.swap(0, Ordering::Relaxed);
                        findings.check(!storage.is_tables() || probes > 0, || {
                            format!("{case} round {round}: no commit probed a table")
                        });
                        let commits = first_commits.swap(0, Ordering::Relaxed);
                        findings.check(commits == expected, || {
                            format!(
                                "{case} round {round}: {commits} of {THREADS} commits \
                                 succeeded at once, expected {expected}"
                            )
                        });
                        let own_keys = (0..THREADS).all(|t| {
                            db.db().get(&unique(round, t)).unwrap() == Some(vec![t as u8])
                        });
                        let shared_key = db.db().get(&fresh(round)).unwrap();
                        findings.check(
                            own_keys && shared_key.as_deref() == Some(SHARED_VALUE),
                            || format!("{case} round {round}: a head is missing"),
                        );
                    }
                }
            });
        }
    });
    findings.judge();
}

#[test]
fn a_head_scan_commits_beside_its_peers_only_inside_a_prefix_at_defra_level() {
    for storage in STORAGES {
        for level in LEVELS {
            for walk in WALKS {
                heads_overlap(level, walk, storage);
            }
        }
    }
}
