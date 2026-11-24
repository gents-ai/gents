//! Transaction bench: correctness under contention first, throughput second.
//!
//! The headline is a lost-update probe. Eight threads each apply 50 increments
//! to one shared counter through `get_for_update` + `put` + `commit`, retrying
//! on conflict or lock timeout, and the bench reports how many of the 400
//! increments actually survive. A serializable execution lands 400/400;
//! anything below that is a lost update and is reported exactly as measured.
//!
//! Timing then covers the commit path itself: uncontended single-thread commit
//! rate, and the successful-commit rate of eight threads fighting over a small
//! hot key set. Both transaction flavors run the identical workload, so any
//! divergence is a property of the engine rather than of the bench.

mod common;

use std::time::{Duration, Instant};

use regolith::{
    IsolationLevel, OptimisticTransactionDb, Transaction, TransactionDb, TransactionError,
    TxnOptions,
};

const COUNTER_KEY: &[u8] = b"txn/counter";
const RACE_THREADS: usize = 8;
const RACE_INCREMENTS: u64 = 50;
const RACE_RUNS: usize = 3;
const EXPECTED: u64 = RACE_THREADS as u64 * RACE_INCREMENTS;
const CONTENDED_THREADS: usize = 8;
const HOT_KEYS: u64 = 8;
const VALUE_LEN: usize = 100;

/// Bounds the retry loop so a livelock surfaces as a reported `abandoned`
/// count instead of hanging the run.
const MAX_ATTEMPTS: u32 = 10_000;

/// The two flavors share no trait in the public API, so the workload is
/// written once against this local one and neither flavor gets its own
/// subtly different copy of the loop.
trait TxnDb: Sync {
    fn begin(&self) -> Transaction;
    fn read(&self, key: &[u8]) -> Option<Vec<u8>>;
}

macro_rules! impl_txn_db {
    ($flavor:ty) => {
        impl TxnDb for $flavor {
            fn begin(&self) -> Transaction {
                self.begin(&TxnOptions::new())
            }

            fn read(&self, key: &[u8]) -> Option<Vec<u8>> {
                self.db()
                    .get(key)
                    .unwrap_or_else(|e| panic!("read {}: {e}", String::from_utf8_lossy(key)))
            }
        }
    };
}

impl_txn_db!(TransactionDb);
impl_txn_db!(OptimisticTransactionDb);

#[derive(Clone, Copy)]
enum Flavor {
    Pessimistic,
    Optimistic,
}

impl Flavor {
    fn name(self) -> &'static str {
        match self {
            Flavor::Pessimistic => "pessimistic",
            Flavor::Optimistic => "optimistic",
        }
    }
}

/// The isolation levels the benchmark sweeps.
///
/// The level decides how much of a transaction's footprint is
/// validated at commit, so it is the knob that trades throughput for
/// the anomalies it refuses. Sweeping it is what makes that trade
/// visible as a number rather than an assertion: `ReadCommitted`
/// validates only what was written, `SnapshotIsolation` adds the keys
/// read for update, `RepeatableRead` adds every point read, and `Serializable`
/// adds the whole read set and so pays for the anti-dependency check that
/// refuses write skew.
const LEVELS: [(IsolationLevel, &str); 5] = [
    (IsolationLevel::ReadCommitted, "read-committed"),
    (IsolationLevel::SnapshotIsolation, "snapshot"),
    (IsolationLevel::RepeatableRead, "repeatable-read"),
    (IsolationLevel::Serializable, "serializable"),
    (IsolationLevel::DefraLevel, "defralevel"),
];

fn with_db<R>(
    flavor: Flavor,
    isolation: IsolationLevel,
    tag: &str,
    f: impl FnOnce(&dyn TxnDb) -> R,
) -> R {
    let tmp = common::TempDb::new(tag);
    let opts = common::default_opts();
    let dir = tmp.dir.display().to_string();
    match flavor {
        Flavor::Pessimistic => f(&TransactionDb::open(tmp.path(), opts)
            .unwrap_or_else(|e| panic!("open pessimistic db at {dir}: {e}"))
            .with_isolation(isolation)),
        Flavor::Optimistic => f(&OptimisticTransactionDb::open(tmp.path(), opts)
            .unwrap_or_else(|e| panic!("open optimistic db at {dir}: {e}"))
            .with_isolation(isolation)),
    }
}

#[derive(Clone, Copy, Default)]
struct Attempts {
    commits: u64,
    conflicts: u64,
    busy: u64,
    engine_errors: u64,
    other_errors: u64,
    abandoned: u64,
}

impl Attempts {
    fn add(&mut self, o: Attempts) {
        self.commits += o.commits;
        self.conflicts += o.conflicts;
        self.busy += o.busy;
        self.engine_errors += o.engine_errors;
        self.other_errors += o.other_errors;
        self.abandoned += o.abandoned;
    }

    fn classify(&mut self, e: &TransactionError) {
        match e {
            TransactionError::Conflict { .. } => self.conflicts += 1,
            TransactionError::Busy(_) => self.busy += 1,
            TransactionError::Engine(_) => self.engine_errors += 1,
            _ => self.other_errors += 1,
        }
    }

    fn json(&self) -> String {
        format!(
            "{{\"commits\":{},\"conflicts\":{},\"busy\":{},\"engine_errors\":{},\"other_errors\":{},\"abandoned\":{}}}",
            self.commits,
            self.conflicts,
            self.busy,
            self.engine_errors,
            self.other_errors,
            self.abandoned
        )
    }
}

fn backoff(attempt: u32) {
    if attempt < 16 {
        std::thread::yield_now();
    } else {
        std::thread::sleep(Duration::from_micros(u64::from(attempt.min(256))));
    }
}

fn decode_counter(raw: Option<Vec<u8>>) -> u64 {
    match raw {
        None => 0,
        Some(bytes) => match <[u8; 8]>::try_from(bytes.as_slice()) {
            Ok(a) => u64::from_le_bytes(a),
            Err(_) => panic!("counter value is {} bytes, expected 8", bytes.len()),
        },
    }
}

/// One read-modify-write of `key`, retried until it commits or the attempt
/// budget runs out.
fn bump(db: &dyn TxnDb, key: &[u8]) -> Attempts {
    let mut acc = Attempts::default();
    for attempt in 0..MAX_ATTEMPTS {
        let tx = db.begin();
        let current = match tx.get_for_update(key) {
            Ok(v) => decode_counter(v),
            Err(e) => {
                acc.classify(&e);
                tx.rollback();
                backoff(attempt);
                continue;
            }
        };
        if let Err(e) = tx.put(key, &(current + 1).to_le_bytes()) {
            acc.classify(&e);
            tx.rollback();
            backoff(attempt);
            continue;
        }
        match tx.commit() {
            Ok(_) => {
                acc.commits += 1;
                return acc;
            }
            Err(e) => {
                acc.classify(&e);
                backoff(attempt);
            }
        }
    }
    acc.abandoned += 1;
    acc
}

/// Run `body` on `threads` threads from a common start, and return what they
/// did with the wall time from that start until the last one is done.
fn fan_out(threads: usize, body: impl Fn(usize) -> Attempts + Sync) -> (Attempts, Duration) {
    let line = common::start::StartLine::new(threads);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|t| {
                let (body, line) = (&body, &line);
                scope.spawn(move || {
                    line.wait();
                    body(t)
                })
            })
            .collect();
        let start = line.start();
        let mut total = Attempts::default();
        for h in handles {
            total.add(
                h.join()
                    .unwrap_or_else(|_| panic!("worker thread panicked")),
            );
        }
        (total, start.elapsed())
    })
}

/// One race over a freshly opened database, so the counter always starts at 0.
struct Race {
    survived: u64,
    attempts: Attempts,
}

impl Race {
    fn survival_pct(&self) -> f64 {
        100.0 * self.survived as f64 / EXPECTED as f64
    }

    fn json(&self) -> String {
        format!(
            "{{\"survived\":{},\"survival_pct\":{:.2},\"attempts\":{}}}",
            self.survived,
            self.survival_pct(),
            self.attempts.json()
        )
    }
}

fn counter_race(db: &dyn TxnDb) -> Race {
    let (attempts, _) = fan_out(RACE_THREADS, |_| {
        let mut acc = Attempts::default();
        for _ in 0..RACE_INCREMENTS {
            acc.add(bump(db, COUNTER_KEY));
        }
        acc
    });
    Race {
        survived: decode_counter(db.read(COUNTER_KEY)),
        attempts,
    }
}

/// The outcome is a thread interleaving, so one sample is not a measurement.
/// Every run is reported alongside the median and the observed spread.
fn correctness(flavor: Flavor, isolation: IsolationLevel, level: &str) -> String {
    let mut runs = Vec::with_capacity(RACE_RUNS);
    let mut pcts = Vec::with_capacity(RACE_RUNS);
    for run in 0..RACE_RUNS {
        let race = with_db(
            flavor,
            isolation,
            &format!("txn-race-{}-{level}-{run}", flavor.name()),
            counter_race,
        );
        println!(
            "{:<12} {level:<15} run {run}: {}/{EXPECTED} increments survived ({:.1}%), \
             attempts: {} commits / {} conflicts / {} busy / {} abandoned",
            flavor.name(),
            race.survived,
            race.survival_pct(),
            race.attempts.commits,
            race.attempts.conflicts,
            race.attempts.busy,
            race.attempts.abandoned
        );
        pcts.push(race.survival_pct());
        runs.push(race.json());
    }
    let (lo, hi) = common::min_max(&pcts);
    let median = common::median(&mut pcts);
    println!(
        "{:<12} survival across {RACE_RUNS} runs: median {median:.1}%, min {lo:.1}%, max {hi:.1}%",
        flavor.name()
    );
    format!(
        "{{\"threads\":{RACE_THREADS},\"increments_per_thread\":{RACE_INCREMENTS},\
         \"expected\":{EXPECTED},\"runs\":[{}],\
         \"survival_pct_median\":{median:.2},\"survival_pct_min\":{lo:.2},\"survival_pct_max\":{hi:.2}}}",
        runs.join(",")
    )
}

fn rate(ops: u64, elapsed: Duration) -> f64 {
    let secs = elapsed.as_secs_f64();
    assert!(secs > 0.0, "timed section reported a zero duration");
    ops as f64 / secs
}

/// Single thread, one distinct key per transaction: the floor cost of
/// begin + put + commit with no conflict handling in the way.
fn uncontended_rate(db: &dyn TxnDb, ops: u64, offset: u64) -> f64 {
    let mut rng = common::Rng::new(0x5EED_0001 ^ offset);
    let value = common::rand_value(&mut rng, VALUE_LEN);
    let start = Instant::now();
    for i in 0..ops {
        let key = common::key(offset + i);
        let tx = db.begin();
        tx.put(&key, &value)
            .unwrap_or_else(|e| panic!("uncontended put: {e}"));
        tx.commit()
            .unwrap_or_else(|e| panic!("uncontended commit: {e}"));
    }
    rate(ops, start.elapsed())
}

fn hot_key(n: u64) -> Vec<u8> {
    format!("txn/hot{:03}", n % HOT_KEYS).into_bytes()
}

/// Every thread walks the same small key set from a different starting
/// offset, so each round has all eight threads overlapping.
fn contended_rate(db: &dyn TxnDb, per_thread: u64) -> (f64, Attempts) {
    let (attempts, elapsed) = fan_out(CONTENDED_THREADS, |t| {
        let mut acc = Attempts::default();
        for i in 0..per_thread {
            acc.add(bump(db, &hot_key(t as u64 + i)));
        }
        acc
    });
    (rate(attempts.commits, elapsed), attempts)
}

struct Timing {
    uncontended: Vec<f64>,
    contended: Vec<f64>,
    contended_attempts: Attempts,
}

impl Timing {
    fn json(&mut self) -> String {
        let (umin, umax) = common::min_max(&self.uncontended);
        let (cmin, cmax) = common::min_max(&self.contended);
        format!(
            "{{\"uncontended_commits_per_s\":{{\"median\":{:.1},\"min\":{umin:.1},\"max\":{umax:.1}}},\
             \"contended_threads\":{CONTENDED_THREADS},\"hot_keys\":{HOT_KEYS},\
             \"contended_commits_per_s\":{{\"median\":{:.1},\"min\":{cmin:.1},\"max\":{cmax:.1}}},\
             \"contended_attempts\":{}}}",
            common::median(&mut self.uncontended),
            common::median(&mut self.contended),
            self.contended_attempts.json()
        )
    }
}

fn timing(db: &dyn TxnDb, reps: usize, uncontended_ops: u64, contended_ops: u64) -> Timing {
    let mut uncontended = Vec::with_capacity(reps);
    let mut contended = Vec::with_capacity(reps);
    let mut contended_attempts = Attempts::default();
    for rep in 0..reps {
        uncontended.push(uncontended_rate(
            db,
            uncontended_ops,
            rep as u64 * uncontended_ops,
        ));
        let (r, a) = contended_rate(db, contended_ops);
        contended.push(r);
        contended_attempts.add(a);
    }
    Timing {
        uncontended,
        contended,
        contended_attempts,
    }
}

fn run(
    flavor: Flavor,
    isolation: IsolationLevel,
    level: &str,
    reps: usize,
    uncontended_ops: u64,
    contended_ops: u64,
) -> (String, String, String) {
    let correctness_json = correctness(flavor, isolation, level);

    let mut rates = with_db(
        flavor,
        isolation,
        &format!("txn-rate-{}-{level}", flavor.name()),
        |db| timing(db, reps, uncontended_ops, contended_ops),
    );
    let timing_json = rates.json();
    println!(
        "{:<12} {level:<15} commits/s: uncontended {:.0}, contended x{} {:.0}",
        flavor.name(),
        common::median(&mut rates.uncontended),
        CONTENDED_THREADS,
        common::median(&mut rates.contended)
    );

    // One flat row per matrix cell, so the dashboard can graph the
    // levels against each other without reaching into the nested
    // correctness and timing documents.
    let a = &rates.contended_attempts;
    let decided = a.commits + a.conflicts + a.busy;
    let conflict_rate = if decided == 0 {
        0.0
    } else {
        100.0 * (a.conflicts + a.busy) as f64 / decided as f64
    };
    let cell_json = format!(
        "{{\"flavor\":\"{}\",\"isolation\":\"{level}\",\
         \"uncontended_commits_per_s\":{:.1},\"contended_commits_per_s\":{:.1},\
         \"conflict_rate_pct\":{conflict_rate:.2}}}",
        flavor.name(),
        common::median(&mut rates.uncontended),
        common::median(&mut rates.contended),
    );

    (correctness_json, timing_json, cell_json)
}

fn main() {
    let quick = std::env::args().any(|a| a == "--quick" || a == "--test");
    let (reps, uncontended_ops, contended_ops) = if quick {
        (2, 1_000, 50)
    } else {
        (5, 20_000, 400)
    };

    println!(
        "transaction bench: {RACE_THREADS} threads x {RACE_INCREMENTS} increments of one counter"
    );
    // The full matrix: every flavor against every isolation level. The
    // point of sweeping both is that they are not independent. A
    // pessimistic lock and a serializable read set refuse overlapping
    // sets of anomalies, so the cost of the stricter level depends on
    // which flavor is already paying for part of it.
    let mut cells = Vec::with_capacity(2 * LEVELS.len());
    let mut rows = Vec::with_capacity(2 * LEVELS.len());
    for flavor in [Flavor::Pessimistic, Flavor::Optimistic] {
        for (isolation, level) in LEVELS {
            let (correctness_json, timing_json, cell_json) = run(
                flavor,
                isolation,
                level,
                reps,
                uncontended_ops,
                contended_ops,
            );
            cells.push(format!(
                "{{\"flavor\":\"{}\",\"isolation\":\"{level}\",\
                 \"correctness\":{correctness_json},\"throughput\":{timing_json}}}",
                flavor.name()
            ));
            rows.push(cell_json);
        }
    }

    common::write_family(
        "transaction",
        &format!(
            "{{\"quick\":{quick},\"reps\":{reps},\"matrix\":[{}]}}",
            cells.join(",")
        ),
    );
    common::write_family(
        "isolation",
        &format!(
            "{{\"unit\":\"commits/s\",\"quick\":{quick},\
             \"note\":\"Every transaction flavor against every isolation level. The level \
             decides how much of a transaction's footprint is validated at commit, so a \
             stricter level refuses more and commits fewer; that refusal is the work it is \
             doing.\",\"matrix\":[{}]}}",
            rows.join(",")
        ),
    );
}
