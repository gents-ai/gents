//! Commits through `commit_nowait` against the blocking `commit`, under
//! concurrency.
//!
//! Each writer commits optimistic transactions that put one 100-byte value
//! under a key of its own, so no commit conflicts. Three ways to commit are
//! swept at Immediate and Eventual durability over 1, 4 and 16 writers:
//!
//! - `commit`: the blocking call, the baseline;
//! - `nowait_1`: `commit_nowait`, then `IoQueue::block_on` of the ticket, one
//!   commit in flight per writer: the price of the ticket and the queue over
//!   the blocking call;
//! - `nowait_16`: up to 16 tickets in flight per writer, the oldest awaited
//!   only when the window is full: what a writer that does not wait on its
//!   own commit gains, since its next commits join the group fsync the
//!   earlier ones wait on.
//!
//! Reported per point: commits per second, and the p50 and p99 of one
//! commit's latency from begin to the outcome delivered. An fsync on a tmpfs
//! costs nothing, so the storage root is printed with the result; point
//! TMPDIR at the device that matters.

mod common;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use regolith::{
    CommitTicket, DurabilityMode, IoQueue, OptimisticTransactionDb, Options, TxnOptions,
};

const WRITERS: [usize; 3] = [1, 4, 16];
const VALUE_BYTES: usize = 100;
const WARMUP_COMMITS: u64 = 8;
/// Each writer owns a disjoint stretch of the keyspace.
const WRITER_STRIDE: u64 = 1_000_000_000;

#[derive(Clone, Copy)]
enum Mode {
    Blocking,
    Nowait { window: usize },
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Blocking => "commit",
            Mode::Nowait { window: 1 } => "nowait_1",
            Mode::Nowait { .. } => "nowait_16",
        }
    }
}

const MODES: [Mode; 3] = [
    Mode::Blocking,
    Mode::Nowait { window: 1 },
    Mode::Nowait { window: 16 },
];

fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|_| panic!("{name}: expected an integer, got {v:?}")),
        Err(_) => default,
    }
}

/// A memtable large enough that a repetition never rotates, so the number is
/// the commit path and not a flush; each repetition checks that it did not.
fn opts(durability: DurabilityMode) -> Options {
    common::default_opts()
        .durability(durability)
        .write_buffer_size(1 << 30)
        .block_cache_size(8 * 1024 * 1024)
        .block_cache_num_shard_bits(0)
}

struct Rep {
    commits_per_sec: f64,
    p50_us: f64,
    p99_us: f64,
}

fn percentile(sorted: &[u32], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    f64::from(sorted[rank.min(sorted.len() - 1)])
}

fn micros_since(start: Instant) -> u32 {
    u32::try_from(start.elapsed().as_micros()).unwrap_or(u32::MAX)
}

/// One writer's loop: commits until `deadline`, returning how many it made
/// and each one's latency.
struct Writer<'a> {
    db: &'a OptimisticTransactionDb,
    queue: IoQueue,
    value: Vec<u8>,
    base: u64,
    next: u64,
    in_flight: VecDeque<(Instant, CommitTicket)>,
}

impl Writer<'_> {
    fn begin_put(&mut self) -> regolith::Transaction {
        let options = TxnOptions::new().io_queue(self.queue.id());
        let tx = self.db.begin(&options);
        tx.put(&common::key(self.base + self.next), &self.value)
            .expect("put");
        self.next += 1;
        tx
    }

    /// Await the oldest ticket in flight, then take every later one already
    /// delivered, recording each latency.
    fn settle_oldest(&mut self, latencies: &mut Vec<u32>) {
        let Some((start, ticket)) = self.in_flight.pop_front() else {
            return;
        };
        self.queue.block_on(ticket).expect("commit");
        latencies.push(micros_since(start));
        while self
            .in_flight
            .front()
            .is_some_and(|(_, ticket)| ticket.is_ready())
        {
            let (start, ticket) = self.in_flight.pop_front().expect("front");
            self.queue.block_on(ticket).expect("commit");
            latencies.push(micros_since(start));
        }
    }

    fn one(&mut self, mode: Mode, latencies: &mut Vec<u32>) {
        let start = Instant::now();
        let tx = self.begin_put();
        match mode {
            Mode::Blocking => {
                tx.commit().expect("commit");
                latencies.push(micros_since(start));
            }
            Mode::Nowait { window } => {
                self.in_flight.push_back((start, tx.commit_nowait()));
                if self.in_flight.len() >= window {
                    self.settle_oldest(latencies);
                }
            }
        }
    }

    fn drain(&mut self, latencies: &mut Vec<u32>) {
        while !self.in_flight.is_empty() {
            self.settle_oldest(latencies);
        }
    }
}

fn run_rep(durability: DurabilityMode, mode: Mode, writers: usize, dur: Duration) -> Rep {
    let tmp = common::TempDb::new("commit-nowait");
    let db = Arc::new(
        OptimisticTransactionDb::open(tmp.path(), opts(durability))
            .unwrap_or_else(|e| panic!("open db at {}: {e}", tmp.path().display())),
    );
    let line = Arc::new(common::start::StartLine::new(writers));
    let committed = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::with_capacity(writers);
    for w in 0..writers {
        let db = Arc::clone(&db);
        let line = Arc::clone(&line);
        let committed = Arc::clone(&committed);
        handles.push(std::thread::spawn(move || {
            let mut rng = common::Rng::new(0x0C0_17A1 ^ w as u64);
            let mut writer = Writer {
                db: &db,
                queue: db.db().io_queue(),
                value: common::rand_value(&mut rng, VALUE_BYTES),
                base: w as u64 * WRITER_STRIDE,
                next: 0,
                in_flight: VecDeque::new(),
            };
            let mut warmup = Vec::new();
            for _ in 0..WARMUP_COMMITS {
                writer.one(mode, &mut warmup);
            }
            writer.drain(&mut warmup);
            // Sized for a fast run so the measured loop does not reallocate.
            let mut latencies: Vec<u32> = Vec::with_capacity(1 << 16);
            let deadline = line.wait() + dur;
            let first = writer.next;
            while Instant::now() < deadline {
                writer.one(mode, &mut latencies);
            }
            writer.drain(&mut latencies);
            committed.fetch_add(writer.next - first, Ordering::Relaxed);
            latencies
        }));
    }
    let start = line.start();
    let mut latencies: Vec<u32> = Vec::new();
    for h in handles {
        latencies.extend(h.join().expect("writer thread panicked"));
    }
    let elapsed = start.elapsed().as_secs_f64();
    latencies.sort_unstable();
    let commits = committed.load(Ordering::Relaxed) as f64;
    assert!(
        !common::rotated(db.db()),
        "a memtable was sealed inside a timed repetition: raise write_buffer_size so every row times the write path alone"
    );
    db.db().close().expect("close db");
    drop(db);
    drop(tmp);
    Rep {
        commits_per_sec: commits / elapsed,
        p50_us: percentile(&latencies, 0.50),
        p99_us: percentile(&latencies, 0.99),
    }
}

struct Summary {
    durability: &'static str,
    mode: &'static str,
    writers: usize,
    median: f64,
    lo: f64,
    hi: f64,
    p50_us: f64,
    p99_us: f64,
}

fn summarize(durability: &'static str, mode: Mode, writers: usize, reps: &[Rep]) -> Summary {
    let mut rate: Vec<f64> = reps.iter().map(|r| r.commits_per_sec).collect();
    let (lo, hi) = common::min_max(&rate);
    let median = common::median(&mut rate);
    let mut p50: Vec<f64> = reps.iter().map(|r| r.p50_us).collect();
    let mut p99: Vec<f64> = reps.iter().map(|r| r.p99_us).collect();
    Summary {
        durability,
        mode: mode.name(),
        writers,
        median,
        lo,
        hi,
        p50_us: common::median(&mut p50),
        p99_us: common::median(&mut p99),
    }
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.3}")
    } else {
        "null".to_string()
    }
}

fn json_str(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() {
    let args = common::args();
    let smoke = args.iter().any(|a| a == "--test" || a == "--quick");
    let reps = if smoke {
        1
    } else {
        env_u64("REGOLITH_BENCH_REPS", 5).max(1)
    } as usize;
    let rep_ms = if smoke {
        50
    } else {
        env_u64("REGOLITH_BENCH_REP_MS", 1000)
    };
    let dur = Duration::from_millis(rep_ms);

    let root = {
        let probe = common::TempDb::new("root-probe");
        let dir = probe.path().parent().unwrap_or_else(|| probe.path());
        dir.display().to_string()
    };
    println!(
        "commit against commit_nowait, {reps} reps x {rep_ms} ms, value {VALUE_BYTES} B, storage root {root}"
    );
    let mut summaries = Vec::new();
    for (name, durability) in [
        ("immediate", DurabilityMode::Immediate),
        ("eventual", DurabilityMode::Eventual),
    ] {
        for writers in WRITERS {
            for mode in MODES {
                let measured: Vec<Rep> = (0..reps)
                    .map(|_| run_rep(durability, mode, writers, dur))
                    .collect();
                let s = summarize(name, mode, writers, &measured);
                println!(
                    "  {:>9} {:>9} {:>2} writer(s): median {:>10.0} commits/s  min {:>10.0}  max {:>10.0}  p50 {:>8.0} us  p99 {:>8.0} us",
                    s.durability, s.mode, s.writers, s.median, s.lo, s.hi, s.p50_us, s.p99_us,
                );
                summaries.push(s);
            }
        }
    }
    if smoke {
        println!("  smoke run (--test): metrics not emitted");
        return;
    }
    let rows: Vec<String> = summaries
        .iter()
        .map(|s| {
            format!(
                "{{\"durability\":\"{}\",\"mode\":\"{}\",\"writers\":{},\"commits_per_sec_median\":{},\"commits_per_sec_min\":{},\"commits_per_sec_max\":{},\"p50_us_median\":{},\"p99_us_median\":{}}}",
                s.durability,
                s.mode,
                s.writers,
                num(s.median),
                num(s.lo),
                num(s.hi),
                num(s.p50_us),
                num(s.p99_us),
            )
        })
        .collect();
    let json = format!(
        "{{\"metric\":\"commit_nowait_throughput\",\"value_bytes\":{VALUE_BYTES},\"reps\":{reps},\"rep_ms\":{rep_ms},\"storage_root\":{},\"rows\":[{}]}}",
        json_str(&root),
        rows.join(",")
    );
    common::write_family("commit_nowait", &json);
}
