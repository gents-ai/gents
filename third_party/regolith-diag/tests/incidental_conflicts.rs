//! A mixed workload is refused only where exactly one may win (plan 7.3).
//!
//! Everything a document store built on `DefraLevel`'s mechanisms does at
//! once, on eight threads, with a key classifier installed. In every round
//! each thread takes one role, all eight begin and buffer their work before
//! any of them commits, and then all commit at once, so every transaction
//! has the other seven land inside it:
//!
//! * two creators each add a document to one collection: they read whether a
//!   block the collection's documents share is stored and store it when it
//!   is not, store a block of their own, scan the collection's heads and add
//!   a head of their own and one both write alike, and append the document
//!   to the collection's arrival log under a once key;
//! * a renewer reads one part of a job's document and writes that part;
//! * an interrupter writes another part of the same document without
//!   reading it;
//! * a meter adds to a usage counter without reading it;
//! * an observer reads the job, the counter, the arrival log and the heads,
//!   and commits without having written anything;
//! * two racers go for the one thing only one of them may have, a different
//!   thing each round: the job (each reads the arrival log, reads the part
//!   that names the job's owner, finds none and writes itself there), a
//!   unique name (each reads it for update, finds none and puts it), or the
//!   closing of a run (each reads its state for update, finds it running and
//!   puts it closed).
//!
//! Of the eight commits exactly one is refused: the racer that lost, with a
//! reason that names what was raced for. Every other refusal is one the
//! mechanisms exist to remove, and fails the test. The loser runs again,
//! finds the winner, and has nothing left to write. After each round the
//! state is what one winner and seven other commits leave, and on `Tables` a
//! flush or a compaction follows, so what the next round reads and what its
//! commits are checked against lives in tables.
//!
//! The same round at `RepeatableRead` refuses work that is no race at all,
//! which is how the rounds are known to overlap.
//!
//! Threads never assert between two rendezvous, since a thread that panicked
//! would strand the rest; they record what they saw and the test judges
//! after the scope has ended.

// Native-only. wasm-pack builds every test target for wasm32, and these use
// threads and the filesystem.
#![cfg(not(target_arch = "wasm32"))]

mod common;
/// The storage shapes, the rendezvous and the collector of the eight-thread
/// gate, which also holds helpers only that gate uses.
#[allow(dead_code)]
#[path = "concurrent_rw_rwr/support.rs"]
mod support;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use common::parted::{PARTS, Parted, add, parts_of, value};
use regolith::prelude::*;
use regolith::{Access, WriteKind};
use support::{Findings, Rendezvous, STORAGES, Storage};

/// How long a rendezvous may take before the test fails instead of hanging.
const HANDOFF: Duration = Duration::from_secs(30);

const THREADS: usize = 8;

/// Rounds of one run: four of each race.
const ROUNDS: usize = 12;

/// The parts of a job's document: when its lease runs out, whether it was
/// asked to stop, and who owns it (zero for nobody).
const LEASE: u32 = 0;
const INTERRUPT: u32 = 1;
const OWNER: u32 = 2;

const USAGE: &[u8] = b"doc/usage";
const COMMON_BLOCK: &[u8] = b"b/common";
const ARRIVALS_HEAD: &[u8] = b"arrivals-head";

/// Blocks are named by their bytes, the collection's heads are added under
/// fresh names, and the arrival log is a log.
struct Classes;

impl KeyClassifier for Classes {
    fn classify(&self, key: &[u8]) -> KeyClass {
        if key.starts_with(b"b/") {
            KeyClass::ContentAddressed
        } else if key.starts_with(b"h/col/") {
            KeyClass::CommutativePrefix { len: 6 }
        } else if key.starts_with(b"arrivals") || key.starts_with(b"once/") {
            KeyClass::Log
        } else {
            KeyClass::Ordinary
        }
    }
}

/// Entries at `arrivals/<20 decimal digits>`, the head at `arrivals-head`.
struct Arrivals;

impl LogLayout for Arrivals {
    fn head_key(&self) -> &[u8] {
        ARRIVALS_HEAD
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(arrival(position).as_slice());
    }

    fn max_entry_key_len(&self) -> usize {
        "arrivals/".len() + 20
    }
}

fn arrival(position: u64) -> Vec<u8> {
    format!("arrivals/{position:020}").into_bytes()
}

/// The job the renewer, the interrupter and, in a claim round, the racers
/// work on in `round`.
fn job(round: usize) -> Vec<u8> {
    format!("doc/job-{round:04}").into_bytes()
}

/// The document creator `t` adds in `round`.
fn created(round: usize, t: usize) -> Vec<u8> {
    format!("doc/new-{round:04}-{t}").into_bytes()
}

/// The block only creator `t` stores in `round`.
fn own_block(round: usize, t: usize) -> Vec<u8> {
    format!("b/{round:04}-{t}").into_bytes()
}

/// The head creator `t` adds in `round`.
fn own_head(round: usize, t: usize) -> Vec<u8> {
    format!("h/col/{round:04}-{t}").into_bytes()
}

/// The head both creators of `round` write, with the same bytes.
fn shared_head(round: usize) -> Vec<u8> {
    format!("h/col/round-{round:04}").into_bytes()
}

fn unique_name(round: usize) -> Vec<u8> {
    format!("unique/{round:04}").into_bytes()
}

fn run_state(round: usize) -> Vec<u8> {
    format!("state/{round:04}").into_bytes()
}

/// What a thread does in a round.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Creator,
    Renewer,
    Interrupter,
    Meter,
    Observer,
    Racer,
}

const ROLES: [Role; THREADS] = [
    Role::Creator,
    Role::Creator,
    Role::Renewer,
    Role::Interrupter,
    Role::Meter,
    Role::Observer,
    Role::Racer,
    Role::Racer,
];

/// The roles turn by one thread each round, so every thread takes each.
fn role_of(t: usize, round: usize) -> Role {
    ROLES[(t + round) % THREADS]
}

/// What the two racers of a round go for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Race {
    /// The job: its owner part, read by parts and written by an operand.
    Claim,
    /// A unique name: read for update, then put.
    Register,
    /// The closing of a run: its state read for update, then put.
    Close,
}

impl Race {
    fn of(round: usize) -> Self {
        [Self::Claim, Self::Register, Self::Close][round % 3]
    }

    /// The key the race is over.
    fn key(self, round: usize) -> Vec<u8> {
        match self {
            Self::Claim => job(round),
            Self::Register => unique_name(round),
            Self::Close => run_state(round),
        }
    }

    /// What the loser's refusal names: what it did with the key, and the
    /// winner's write.
    fn reason(self) -> (Access, WriteKind) {
        match self {
            Self::Claim => (Access::ReadParts, WriteKind::Merge),
            Self::Register | Self::Close => (Access::ReadForUpdate, WriteKind::Put),
        }
    }
}

/// What `closed by` thread `t` leaves in a run's state.
fn closed_by(t: usize) -> Vec<u8> {
    format!("closed by {t}").into_bytes()
}

/// Begin at `level` and do everything thread `t`'s role does in `round`
/// short of committing.
fn stage(
    db: &OptimisticTransactionDb,
    level: IsolationLevel,
    round: usize,
    t: usize,
) -> TxResult<Transaction> {
    let tx = db.begin(&TxnOptions::new().isolation(level));
    let job = job(round);
    match role_of(t, round) {
        Role::Creator => {
            if tx.get(COMMON_BLOCK)?.is_none() {
                tx.put(COMMON_BLOCK, COMMON_BLOCK)?;
            }
            tx.put(&own_block(round, t), &own_block(round, t))?;
            for head in tx.scan_stream(Some(b"h/col/"), Some(b"h/col0")) {
                head?;
            }
            tx.put(&own_head(round, t), b"head")?;
            tx.put(&shared_head(round), b"head")?;
            tx.put(&created(round, t), &value([0; PARTS]))?;
            let log: Arc<dyn LogLayout> = Arc::new(Arrivals);
            let mut once = b"once/".to_vec();
            once.extend_from_slice(&created(round, t));
            tx.append(&log, &created(round, t), Some(&once))?;
        }
        Role::Renewer => {
            tx.get_parts(&job, &[LEASE])?;
            tx.merge(&job, &add(LEASE, 1))?;
        }
        Role::Interrupter => tx.merge(&job, &add(INTERRUPT, 1))?,
        Role::Meter => tx.merge(USAGE, &add(0, 1))?,
        Role::Observer => {
            tx.get(&job)?;
            tx.get(USAGE)?;
            tx.get(ARRIVALS_HEAD)?;
            tx.get(&run_state(round))?;
            tx.get(&unique_name(round))?;
            for head in tx.scan_stream(Some(b"h/col/"), Some(b"h/col0")) {
                head?;
            }
        }
        Role::Racer => match Race::of(round) {
            Race::Claim => {
                // A consumer of the arrival log: it reads the log before it
                // takes the job, and decides nothing on what the log held.
                if tx.get(ARRIVALS_HEAD)?.is_some() {
                    tx.get(&arrival(1))?;
                }
                let owner = tx
                    .get_parts(&job, &[OWNER])?
                    .map_or(0, |bytes| parts_of(&bytes).expect("a job")[OWNER as usize]);
                if owner == 0 {
                    tx.merge(&job, &add(OWNER, t as u64 + 1))?;
                }
            }
            Race::Register => {
                if tx.get_for_update(&unique_name(round))?.is_none() {
                    tx.put(&unique_name(round), &[t as u8])?;
                }
            }
            Race::Close => {
                if tx.get_for_update(&run_state(round))?.as_deref() == Some(&b"running"[..]) {
                    tx.put(&run_state(round), &closed_by(t))?;
                }
            }
        },
    }
    Ok(tx)
}

/// What one thread's first commit of a round came to.
#[derive(Clone, Debug)]
enum Outcome {
    Committed,
    Refused {
        key: Vec<u8>,
        mine: Access,
        theirs: WriteKind,
    },
}

/// What a run saw: the refusals of commits that were no race, and how many
/// races were lost.
#[derive(Default)]
struct Tally {
    incidental: Mutex<Vec<String>>,
    races_lost: AtomicU64,
}

fn open(dir: &tempfile::TempDir, storage: Storage) -> OptimisticTransactionDb {
    let db = OptimisticTransactionDb::open(
        dir.path(),
        storage.options().merge_operator(Some(Arc::new(Parted))),
    )
    .unwrap()
    .with_policy(Arc::new(Classes));
    for round in 0..ROUNDS {
        db.db().put(&job(round), &value([0; PARTS])).unwrap();
        db.db().put(&run_state(round), b"running").unwrap();
    }
    // A head before and a head after every head the creators add, so a scan
    // of the heads walks one stretch over all of them.
    for head in [&b"h/col/!"[..], b"h/col/~"] {
        db.db().put(head, b"head").unwrap();
    }
    storage.flush(db.db());
    db
}

/// The state after `round`, checked against what one winner and seven other
/// commits leave. `winner` is the racer that committed.
fn judge_round(case: &str, db: &Db, round: usize, winner: Option<usize>, findings: &Findings) {
    let stored = |key: &[u8]| db.get(key).unwrap();
    let parts = |key: &[u8]| stored(key).map(|bytes| parts_of(&bytes).expect("three parts"));
    let race = Race::of(round);
    let owner = match (race, winner) {
        (Race::Claim, Some(winner)) => winner as u64 + 1,
        _ => 0,
    };
    let held = parts(&job(round));
    findings.check(held == Some([1, 1, owner]), || {
        format!("{case} round {round}: the job is {held:?}, expected one renewal, one interrupt and owner {owner}")
    });
    let used = parts(USAGE).map_or(0, |parts| parts[0]);
    findings.check(used == round as u64 + 1, || {
        format!("{case} round {round}: the usage counter is {used}")
    });
    if let Some(winner) = winner {
        let (name, state) = (stored(&unique_name(round)), stored(&run_state(round)));
        let (expected_name, expected_state) = match race {
            Race::Claim => (None, b"running".to_vec()),
            Race::Register => (Some(vec![winner as u8]), b"running".to_vec()),
            Race::Close => (None, closed_by(winner)),
        };
        findings.check(
            name == expected_name && state == Some(expected_state),
            || format!("{case} round {round}: the name is {name:?} and the state {state:?}"),
        );
    }
    let creators: Vec<usize> = (0..THREADS)
        .filter(|t| role_of(*t, round) == Role::Creator)
        .collect();
    for &t in &creators {
        let all_there = stored(&created(round, t)).is_some()
            && stored(&own_block(round, t)).is_some()
            && stored(&own_head(round, t)).is_some();
        findings.check(all_there, || {
            format!("{case} round {round}: creator {t} left no document, block or head")
        });
    }
    let shared = stored(COMMON_BLOCK).is_some() && stored(&shared_head(round)).is_some();
    findings.check(shared, || {
        format!("{case} round {round}: the shared block or the shared head is missing")
    });
    let head = stored(ARRIVALS_HEAD).map_or(0, |raw| {
        u64::from_be_bytes(raw.as_slice().try_into().expect("an 8-byte head"))
    });
    findings.check(head == 2 * (round as u64 + 1), || {
        format!("{case} round {round}: the arrival log's head is {head}")
    });
}

/// Run `rounds` rounds at `level` and return what was refused. With `judge`
/// the state is checked after every round and at the end.
fn run(level: IsolationLevel, storage: Storage, rounds: usize, judge: bool) -> Tally {
    let case = format!("{storage:?} {level:?}");
    let dir = tempfile::tempdir().unwrap();
    let db = open(&dir, storage);
    let sync = Rendezvous::new(THREADS);
    let findings = Findings::default();
    let tally = Tally::default();
    let outcomes: Mutex<Vec<Option<Outcome>>> = Mutex::new(vec![None; THREADS]);

    thread::scope(|scope| {
        for t in 0..THREADS {
            let (case, db, sync, findings) = (&case, &db, &sync, &findings);
            let (tally, outcomes) = (&tally, &outcomes);
            scope.spawn(move || {
                for round in 0..rounds {
                    let role = role_of(t, round);
                    sync.wait(); // the previous round is judged
                    let staged = stage(db, level, round, t);
                    sync.wait(); // all eight have read and buffered
                    let outcome = match staged.and_then(Transaction::commit) {
                        Ok(_) => Outcome::Committed,
                        Err(TransactionError::Conflict(conflict)) => Outcome::Refused {
                            key: conflict.key().to_vec(),
                            mine: conflict.mine(),
                            theirs: conflict.theirs(),
                        },
                        Err(other) => panic!("{case} round {round} {role:?}: {other}"),
                    };
                    outcomes.lock().unwrap()[t] = Some(outcome.clone());
                    sync.wait(); // all eight first commits are in

                    if let Outcome::Refused { key, mine, theirs } = &outcome {
                        let race = Race::of(round);
                        let lost_the_race = role == Role::Racer
                            && *key == race.key(round)
                            && (*mine, *theirs) == race.reason();
                        if lost_the_race {
                            tally.races_lost.fetch_add(1, Ordering::Relaxed);
                        } else {
                            tally.incidental.lock().unwrap().push(format!(
                                "{case} round {round}: the {role:?} was refused on a \
                                 {}-byte key as {mine:?} against a {theirs:?}",
                                key.len()
                            ));
                        }
                        // It runs again and commits what is left to do: for
                        // the loser of a race, nothing.
                        let again = stage(db, level, round, t).and_then(Transaction::commit);
                        findings.check(again.is_ok() || !judge, || {
                            format!(
                                "{case} round {round}: the {role:?} was refused twice: {again:?}"
                            )
                        });
                    }
                    sync.wait(); // every re-run is in

                    if t == 0 {
                        let seen =
                            std::mem::replace(&mut *outcomes.lock().unwrap(), vec![None; THREADS]);
                        let winners: Vec<usize> = (0..THREADS)
                            .filter(|t| role_of(*t, round) == Role::Racer)
                            .filter(|t| matches!(seen[*t], Some(Outcome::Committed)))
                            .collect();
                        if judge {
                            findings.check(winners.len() == 1, || {
                                format!(
                                    "{case} round {round}: racers {winners:?} won the {:?}",
                                    Race::of(round)
                                )
                            });
                            judge_round(case, db.db(), round, winners.first().copied(), findings);
                        }
                        if storage.is_tables() {
                            db.db().flush().unwrap();
                            if round % 2 == 1 {
                                db.db().compact_range(None, None).wait().unwrap();
                            }
                        }
                    }
                }
            });
        }
    });

    if judge {
        let log: Vec<Option<Vec<u8>>> = (1..=2 * rounds as u64)
            .map(|position| db.db().get(&arrival(position)).unwrap())
            .collect();
        let mut arrived: Vec<Vec<u8>> = log.iter().flatten().cloned().collect();
        arrived.sort_unstable();
        let mut documents: Vec<Vec<u8>> = (0..rounds)
            .flat_map(|round| {
                (0..THREADS)
                    .filter(move |t| role_of(*t, round) == Role::Creator)
                    .map(move |t| created(round, t))
            })
            .collect();
        documents.sort_unstable();
        findings.check(arrived == documents, || {
            format!("{case}: the arrival log does not hold every document once, in a dense run")
        });
        // Operands fold in a compaction; the jobs stay what they were.
        let jobs = |db: &Db| -> Vec<Option<Vec<u8>>> {
            (0..rounds)
                .map(|round| db.get(&job(round)).unwrap())
                .collect()
        };
        let before = jobs(db.db());
        db.db().compact_range(None, None).wait().unwrap();
        findings.check(jobs(db.db()) == before, || {
            format!("{case}: a compaction changed a job")
        });
    }
    findings.judge();
    tally
}

#[test]
fn a_mixed_workload_is_refused_only_where_exactly_one_may_win() {
    for storage in STORAGES {
        let tally = run(IsolationLevel::DefraLevel, storage, ROUNDS, true);
        let incidental = tally.incidental.into_inner().unwrap();
        assert!(
            incidental.is_empty(),
            "{storage:?}: {} commit(s) that were no race were refused, the first {}:\n{}",
            incidental.len(),
            incidental.len().min(8),
            incidental[..incidental.len().min(8)].join("\n")
        );
        assert_eq!(
            tally.races_lost.load(Ordering::Relaxed),
            ROUNDS as u64,
            "{storage:?}: every round has one race, and one racer loses it"
        );
    }
}

/// The control: one round of the same mix where no class and no part is
/// consulted. Work that is no race is refused, so the eight do overlap.
#[test]
fn below_defra_level_the_same_round_refuses_work_that_is_no_race() {
    for storage in STORAGES {
        let tally = run(IsolationLevel::RepeatableRead, storage, 1, false);
        let incidental = tally.incidental.into_inner().unwrap();
        assert!(
            !incidental.is_empty(),
            "{storage:?}: nothing but the race was refused at RepeatableRead, so the round \
             did not overlap"
        );
    }
}
