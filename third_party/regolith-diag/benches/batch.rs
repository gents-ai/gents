//! `WriteBatch` throughput at 1/8/32/256 operations per batch, plus a bulk
//! load reported in MiB/s.
//!
//! The batch sweep answers "what does grouping buy": one op per batch is the
//! same work as `put`, and the curve from there shows how much of a write is
//! per-batch overhead (WAL record framing, sequence allocation, the write
//! lock) rather than per-key work.
//!
//! The bulk figure times a fixed payload written through large batches and
//! includes the closing flush, so it is bytes landed on disk per second, not
//! bytes parked in the memtable. Payload bytes are key plus value; the WAL and
//! SSTable encoding around them are not counted.

mod common;

use std::time::{Duration, Instant};

use regolith::{Options, WriteBatch};

const BATCH_SIZES: [usize; 4] = [1, 8, 32, 256];
const VALUE_BYTES: usize = 100;
const BULK_VALUE_BYTES: usize = 1024;
const BULK_BATCH_OPS: usize = 256;
const WARMUP_BATCHES: u64 = 8;
const UNSTABLE_SPREAD: f64 = 0.5;

fn env_u64(name: &str, default: u64) -> u64 {
    match std::env::var(name) {
        Ok(v) => v
            .parse()
            .unwrap_or_else(|_| panic!("{name}: expected an integer, got {v:?}")),
        Err(_) => default,
    }
}

/// One cache shard keeps RSS bounded across the sweep; a memtable larger than
/// any repetition writes keeps it off the flush path, so the number is the
/// batch write path itself, and each repetition checks that it was. The
/// memtable grows only with what a repetition writes.
fn opts() -> Options {
    common::default_opts()
        .write_buffer_size(1 << 30)
        .block_cache_size(8 * 1024 * 1024)
        .block_cache_num_shard_bits(0)
}

fn batch_rep(batch_ops: usize, dur: Duration) -> f64 {
    let (tmp, db) = common::open("batch", opts());
    let mut rng = common::Rng::new(0xBA7C_0001 ^ batch_ops as u64);
    let value = common::rand_value(&mut rng, VALUE_BYTES);
    let mut next: u64 = 0;
    let fill = |next: &mut u64| {
        let mut wb = WriteBatch::new();
        for _ in 0..batch_ops {
            wb.put(&common::key(*next), &value);
            *next += 1;
        }
        wb
    };
    for _ in 0..WARMUP_BATCHES {
        db.write(fill(&mut next)).expect("warmup batch");
    }
    let start = Instant::now();
    let deadline = start + dur;
    let mut batches: u64 = 0;
    while Instant::now() < deadline {
        db.write(fill(&mut next)).expect("batch write");
        batches += 1;
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(
        !common::rotated(&db),
        "a memtable was sealed inside a timed repetition: raise write_buffer_size so every row times the write path alone"
    );
    db.close().expect("close db");
    drop(db);
    drop(tmp);
    (batches * batch_ops as u64) as f64 / elapsed
}

/// Returns MiB of key+value payload per second, timed across the batched
/// writes and the closing flush that lands them in an SSTable.
fn bulk_rep(payload_mib: u64) -> f64 {
    let (tmp, db) = common::open("batch-bulk", opts());
    let mut rng = common::Rng::new(0xB01C_0001);
    let value = common::rand_value(&mut rng, BULK_VALUE_BYTES);
    let per_op = (common::key(0).len() + BULK_VALUE_BYTES) as u64;
    let target = payload_mib * 1024 * 1024;
    let total_ops = target / per_op;
    let start = Instant::now();
    let mut next: u64 = 0;
    while next < total_ops {
        let mut wb = WriteBatch::new();
        for _ in 0..BULK_BATCH_OPS.min((total_ops - next) as usize) {
            wb.put(&common::key(next), &value);
            next += 1;
        }
        db.write(wb).expect("bulk batch write");
    }
    db.close().expect("close db");
    let elapsed = start.elapsed().as_secs_f64();
    drop(db);
    drop(tmp);
    (next * per_op) as f64 / (1024.0 * 1024.0) / elapsed
}

struct Summary {
    label: u64,
    median: f64,
    lo: f64,
    hi: f64,
    spread: Option<f64>,
    stability: &'static str,
}

fn summarize(label: u64, mut samples: Vec<f64>) -> Summary {
    let (lo, hi) = common::min_max(&samples);
    let median = common::median(&mut samples);
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
    Summary {
        label,
        median,
        lo,
        hi,
        spread,
        stability,
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

fn row(key: &str, s: &Summary, extra: &str) -> String {
    format!(
        "{{\"{key}\":{},{extra}\"median\":{},\"min\":{},\"max\":{},\"spread_ratio\":{},\"stability\":\"{}\"}}",
        s.label,
        num(s.median),
        num(s.lo),
        num(s.hi),
        opt_num(s.spread),
        s.stability,
    )
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
        env_u64("REGOLITH_BENCH_REPS", 7)
    } as usize;
    let rep_ms = if smoke {
        60
    } else {
        env_u64("REGOLITH_BENCH_REP_MS", 400)
    };
    let bulk_mib = if smoke {
        2
    } else {
        env_u64("REGOLITH_BENCH_BULK_MIB", 32)
    };
    let dur = Duration::from_millis(rep_ms);

    let root = storage_root();
    println!("write batches, {reps} reps x {rep_ms} ms, value {VALUE_BYTES} B");
    println!("  storage root: {root}");
    let mut batch_summaries = Vec::with_capacity(BATCH_SIZES.len());
    for batch_ops in BATCH_SIZES {
        let samples: Vec<f64> = (0..reps).map(|_| batch_rep(batch_ops, dur)).collect();
        let s = summarize(batch_ops as u64, samples);
        println!(
            "  {:>3} op(s)/batch: median {:>10.0} ops/s  min {:>10.0}  max {:>10.0}  spread {}  {}",
            s.label,
            s.median,
            s.lo,
            s.hi,
            s.spread
                .map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
            s.stability,
        );
        batch_summaries.push(s);
    }

    let bulk_reps = if smoke { 1 } else { reps.min(5) };
    let samples: Vec<f64> = (0..bulk_reps).map(|_| bulk_rep(bulk_mib)).collect();
    let bulk = summarize(bulk_mib, samples);
    println!(
        "  bulk load {} MiB payload ({} B values, {} ops/batch, flush included): median {:.1} MiB/s  min {:.1}  max {:.1}  {}",
        bulk_mib, BULK_VALUE_BYTES, BULK_BATCH_OPS, bulk.median, bulk.lo, bulk.hi, bulk.stability,
    );

    if smoke {
        println!("  smoke run (--test): metrics not emitted");
        return;
    }
    let root_json = json_str(&root);
    let rows: Vec<String> = batch_summaries
        .iter()
        .map(|s| row("batch_ops", s, "\"unit\":\"ops_per_sec\","))
        .collect();
    let json = format!(
        "{{\"metric\":\"write_batch\",\"value_bytes\":{VALUE_BYTES},\"reps\":{reps},\"rep_ms\":{rep_ms},\"unstable_spread_threshold\":{UNSTABLE_SPREAD},\"storage_root\":{root_json},\"by_batch_size\":[{}],\"bulk_load\":{}}}",
        rows.join(","),
        row(
            "payload_mib",
            &bulk,
            &format!(
                "\"unit\":\"mib_per_sec\",\"reps\":{bulk_reps},\"value_bytes\":{BULK_VALUE_BYTES},\"batch_ops\":{BULK_BATCH_OPS},\"includes_close_flush\":true,"
            ),
        ),
    );
    common::write_family("batch", &json);
}
