//! Buffered write throughput: `WriteOptions { sync: false }` at 1/2/4/8
//! threads, with the CPU cost of each write.
//!
//! Wall-clock throughput on this path was as jumpy as the durable path on the
//! baseline host, so it gets the same treatment: at least 7 repetitions, min
//! and max next to the median, and a stability verdict, never a bare median.
//!
//! CPU-seconds per million operations is the metric that survives that noise
//! and exposes lock contention: it rose from 3.00 at 1 thread to 5.00 at 8 on
//! the baseline host, meaning eight threads burn 1.7x the CPU to do the same
//! work. It is process-wide (background compaction included), which is the
//! point: the whole cost of a write is charged to the write.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use regolith::{Options, WriteOptions};

const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 8];
const VALUE_BYTES: usize = 100;
const WARMUP_OPS: u64 = 512;
/// Each thread owns a disjoint decade of the keyspace; `common::key` stays
/// fixed-width as long as a thread never writes more than this many keys.
const THREAD_STRIDE: u64 = 1_000_000_000;
/// A buffered put is on the order of a microsecond, so the deadline is only
/// read once per this many ops to keep the clock out of the measurement.
const CLOCK_EVERY: u64 = 64;
const UNSTABLE_SPREAD: f64 = 0.5;
const CPU_METRIC: bool = cfg!(target_os = "linux");

fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|_| panic!("{name}: expected an integer, got {v:?}")),
        Err(_) => default,
    }
}

/// One cache shard and a memtable large enough that a repetition stays inside
/// it, so the number measures the memtable and WAL append path rather than a
/// flush storm; each repetition checks that it did. The memtable grows only
/// with what a repetition writes, and each repetition's database is dropped,
/// so RSS stays bounded across the whole sweep.
fn opts() -> Options {
    common::default_opts()
        .write_buffer_size(1 << 30)
        .block_cache_size(8 * 1024 * 1024)
        .block_cache_num_shard_bits(0)
}

struct Rep {
    ops_per_sec: f64,
    cpu_delta: f64,
    ops: f64,
}

fn run_rep(threads: usize, dur: Duration) -> Rep {
    let (tmp, db) = common::open("write-buffered", opts());
    let db = Arc::new(db);
    let line = Arc::new(common::start::StartLine::new(threads));
    let written = Arc::new(AtomicU64::new(0));
    let mut handles = Vec::with_capacity(threads);
    for t in 0..threads {
        let db = Arc::clone(&db);
        let line = Arc::clone(&line);
        let written = Arc::clone(&written);
        handles.push(std::thread::spawn(move || {
            let wo = WriteOptions::default();
            let mut rng = common::Rng::new(0xB0FF_0001 ^ t as u64);
            let value = common::rand_value(&mut rng, VALUE_BYTES);
            let base = t as u64 * THREAD_STRIDE;
            for i in 0..WARMUP_OPS {
                db.put_opt(&wo, &common::key(base + i), &value)
                    .expect("warmup put");
            }
            let deadline = line.wait() + dur;
            let mut i = WARMUP_OPS;
            loop {
                for _ in 0..CLOCK_EVERY {
                    db.put_opt(&wo, &common::key(base + i), &value)
                        .expect("buffered put");
                    i += 1;
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            written.fetch_add(i - WARMUP_OPS, Ordering::Relaxed);
        }));
    }
    line.all_ready();
    let cpu0 = common::cpu_seconds();
    let start = line.go();
    for h in handles {
        h.join().expect("writer thread panicked");
    }
    let elapsed = start.elapsed().as_secs_f64();
    let cpu_delta = common::cpu_seconds() - cpu0;
    let ops = written.load(Ordering::Relaxed) as f64;
    assert!(
        !common::rotated(&db),
        "a memtable was sealed inside a timed repetition: raise write_buffer_size so every row times the write path alone"
    );
    db.close().expect("close db");
    drop(db);
    drop(tmp);
    Rep {
        ops_per_sec: ops / elapsed,
        cpu_delta,
        ops,
    }
}

struct Summary {
    threads: usize,
    median: f64,
    lo: f64,
    hi: f64,
    spread: Option<f64>,
    stability: &'static str,
    cpu_per_mop: Option<f64>,
    cpu_per_mop_lo: Option<f64>,
    cpu_per_mop_hi: Option<f64>,
}

fn summarize(threads: usize, reps: &[Rep]) -> Summary {
    let mut rate: Vec<f64> = reps.iter().map(|r| r.ops_per_sec).collect();
    let (lo, hi) = common::min_max(&rate);
    let median = common::median(&mut rate);
    let (spread, stability) = if median.is_finite() && median > 0.0 {
        let s = (hi - lo) / median;
        (
            Some(s),
            if s > UNSTABLE_SPREAD {
                "unstable"
            } else {
                "stable"
            },
        )
    } else {
        (None, "unknown")
    };
    let cpu_ok = CPU_METRIC && reps.iter().all(|r| r.cpu_delta > 0.0 && r.ops > 0.0);
    let (cpu_per_mop, cpu_lo, cpu_hi) = if cpu_ok {
        let mut v: Vec<f64> = reps
            .iter()
            .map(|r| r.cpu_delta / (r.ops / 1_000_000.0))
            .collect();
        let (l, h) = common::min_max(&v);
        (Some(common::median(&mut v)), Some(l), Some(h))
    } else {
        (None, None, None)
    };
    Summary {
        threads,
        median,
        lo,
        hi,
        spread,
        stability,
        cpu_per_mop,
        cpu_per_mop_lo: cpu_lo,
        cpu_per_mop_hi: cpu_hi,
    }
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.3}")
    } else {
        "null".to_string()
    }
}

fn opt_num(x: Option<f64>) -> String {
    x.map_or_else(|| "null".to_string(), num)
}

/// The directory the temporary databases land in. A tmpfs and a real device
/// give numbers that differ by orders of magnitude, so the root is reported
/// with the result rather than left to the reader to guess.
fn storage_root() -> String {
    let probe = common::TempDb::new("root-probe");
    let dir = probe.path().parent().unwrap_or_else(|| probe.path());
    dir.display().to_string()
}

fn json_str(s: &str) -> String {
    let clean: String = s.chars().filter(|c| !c.is_control()).collect();
    format!("\"{}\"", clean.replace('\\', "\\\\").replace('"', "\\\""))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let smoke = args.iter().any(|a| a == "--test" || a == "--quick");
    let reps = if smoke {
        2
    } else {
        env_u64("REGOLITH_BENCH_REPS", 9).max(7)
    } as usize;
    let rep_ms = if smoke {
        60
    } else {
        env_u64("REGOLITH_BENCH_REP_MS", 500)
    };
    let dur = Duration::from_millis(rep_ms);

    let root = storage_root();
    println!("buffered writes (sync=false), {reps} reps x {rep_ms} ms, value {VALUE_BYTES} B");
    println!("  storage root: {root}");
    let mut summaries = Vec::with_capacity(THREAD_COUNTS.len());
    for threads in THREAD_COUNTS {
        let measured: Vec<Rep> = (0..reps).map(|_| run_rep(threads, dur)).collect();
        let s = summarize(threads, &measured);
        let cpu = match (s.cpu_per_mop, s.cpu_per_mop_lo, s.cpu_per_mop_hi) {
            (Some(c), Some(l), Some(h)) => format!("{c:.2} cpu-s/Mop (min {l:.2} max {h:.2})"),
            _ if CPU_METRIC => "cpu-s/Mop not resolvable".to_string(),
            _ => "cpu-s/Mop not available on this platform".to_string(),
        };
        println!(
            "  {:>2} thread(s): median {:>10.0} ops/s  min {:>10.0}  max {:>10.0}  spread {}  {}  [{}]",
            s.threads,
            s.median,
            s.lo,
            s.hi,
            s.spread
                .map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
            s.stability,
            cpu,
        );
        summaries.push(s);
    }

    let overall = if summaries.iter().any(|s| s.stability == "unknown") {
        "unknown"
    } else if summaries.iter().any(|s| s.stability == "unstable") {
        "unstable"
    } else {
        "stable"
    };
    println!("  stability: {overall}");

    if smoke {
        println!("  smoke run (--test): metrics not emitted");
        return;
    }
    let root_json = json_str(&root);
    let rows: Vec<String> = summaries
        .iter()
        .map(|s| {
            format!(
                "{{\"threads\":{},\"ops_per_sec_median\":{},\"ops_per_sec_min\":{},\"ops_per_sec_max\":{},\"spread_ratio\":{},\"stability\":\"{}\",\"cpu_s_per_mop_median\":{},\"cpu_s_per_mop_min\":{},\"cpu_s_per_mop_max\":{}}}",
                s.threads,
                num(s.median),
                num(s.lo),
                num(s.hi),
                opt_num(s.spread),
                s.stability,
                opt_num(s.cpu_per_mop),
                opt_num(s.cpu_per_mop_lo),
                opt_num(s.cpu_per_mop_hi),
            )
        })
        .collect();
    let json = format!(
        "{{\"metric\":\"buffered_write_throughput\",\"sync\":false,\"value_bytes\":{VALUE_BYTES},\"reps\":{reps},\"rep_ms\":{rep_ms},\"unstable_spread_threshold\":{UNSTABLE_SPREAD},\"cpu_metric_available\":{CPU_METRIC},\"storage_root\":{root_json},\"stability\":\"{overall}\",\"by_threads\":[{}]}}",
        rows.join(",")
    );
    common::write_family("write_buffered", &json);
}
