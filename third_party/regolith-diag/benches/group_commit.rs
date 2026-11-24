//! Optimistic commit throughput and latency under concurrency.
//!
//! Each writer commits optimistic transactions that put one 100-byte value
//! under a key of its own, so no commit conflicts and every commit is a write
//! the pipeline has to make durable. The sweep runs 1, 4, 16 and 64 writers
//! at Immediate durability (an fsync per commit group) and at Eventual, and
//! reports commits per second with the p50 and p99 of one commit's latency,
//! begin to receipt.
//!
//! Group commit is what this measures: at Immediate a commit that waits for
//! its own fsync alone caps the rate at one fsync per commit whatever the
//! writer count, and a commit that shares a group's fsync does not. An fsync
//! on a tmpfs costs nothing, so the storage root is printed with the result;
//! point TMPDIR at the device that matters.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use regolith::{DurabilityMode, OptimisticTransactionDb, Options, TxnOptions};

const WRITERS: [usize; 4] = [1, 4, 16, 64];
const VALUE_BYTES: usize = 100;
const WARMUP_COMMITS: u64 = 8;
/// Each writer owns a disjoint stretch of the keyspace.
const WRITER_STRIDE: u64 = 1_000_000_000;

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

fn run_rep(durability: DurabilityMode, writers: usize, dur: Duration) -> Rep {
    let tmp = common::TempDb::new("group-commit");
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
            let mut rng = common::Rng::new(0x6C0_0001 ^ w as u64);
            let value = common::rand_value(&mut rng, VALUE_BYTES);
            let base = w as u64 * WRITER_STRIDE;
            let commit = |i: u64| {
                let tx = db.begin(&TxnOptions::new());
                tx.put(&common::key(base + i), &value).expect("put");
                tx.commit().expect("commit");
            };
            for i in 0..WARMUP_COMMITS {
                commit(i);
            }
            // Sized for a fast run so the measured loop does not reallocate.
            let mut latencies: Vec<u32> = Vec::with_capacity(1 << 16);
            let deadline = line.wait() + dur;
            let mut i = WARMUP_COMMITS;
            loop {
                let start = Instant::now();
                if start >= deadline {
                    break;
                }
                commit(i);
                let micros = start.elapsed().as_micros();
                latencies.push(u32::try_from(micros).unwrap_or(u32::MAX));
                i += 1;
            }
            committed.fetch_add(i - WARMUP_COMMITS, Ordering::Relaxed);
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
    writers: usize,
    median: f64,
    lo: f64,
    hi: f64,
    p50_us: f64,
    p99_us: f64,
}

fn summarize(durability: &'static str, writers: usize, reps: &[Rep]) -> Summary {
    let mut rate: Vec<f64> = reps.iter().map(|r| r.commits_per_sec).collect();
    let (lo, hi) = common::min_max(&rate);
    let median = common::median(&mut rate);
    let mut p50: Vec<f64> = reps.iter().map(|r| r.p50_us).collect();
    let mut p99: Vec<f64> = reps.iter().map(|r| r.p99_us).collect();
    Summary {
        durability,
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
    // One durability or one writer count, for an interleaved comparison run.
    let only_durability = std::env::var("REGOLITH_BENCH_DURABILITY").ok();
    let only_writers = std::env::var("REGOLITH_BENCH_WRITERS")
        .ok()
        .map(|v| v.parse::<usize>().expect("REGOLITH_BENCH_WRITERS"));

    let root = {
        let probe = common::TempDb::new("root-probe");
        let dir = probe.path().parent().unwrap_or_else(|| probe.path());
        dir.display().to_string()
    };
    println!(
        "optimistic commits, {reps} reps x {rep_ms} ms, value {VALUE_BYTES} B, storage root {root}"
    );
    let mut summaries = Vec::new();
    for (name, durability) in [
        ("immediate", DurabilityMode::Immediate),
        ("eventual", DurabilityMode::Eventual),
    ] {
        if only_durability.as_deref().is_some_and(|only| only != name) {
            continue;
        }
        for writers in WRITERS {
            if only_writers.is_some_and(|only| only != writers) {
                continue;
            }
            let measured: Vec<Rep> = (0..reps)
                .map(|_| run_rep(durability, writers, dur))
                .collect();
            let s = summarize(name, writers, &measured);
            println!(
                "  {:>9} {:>2} writer(s): median {:>10.0} commits/s  min {:>10.0}  max {:>10.0}  p50 {:>8.0} us  p99 {:>8.0} us",
                s.durability, s.writers, s.median, s.lo, s.hi, s.p50_us, s.p99_us,
            );
            summaries.push(s);
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
                "{{\"durability\":\"{}\",\"writers\":{},\"commits_per_sec_median\":{},\"commits_per_sec_min\":{},\"commits_per_sec_max\":{},\"p50_us_median\":{},\"p99_us_median\":{}}}",
                s.durability,
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
        "{{\"metric\":\"optimistic_commit_throughput\",\"value_bytes\":{VALUE_BYTES},\"reps\":{reps},\"rep_ms\":{rep_ms},\"storage_root\":{},\"rows\":[{}]}}",
        json_str(&root),
        rows.join(",")
    );
    common::write_family("group_commit", &json);
}
