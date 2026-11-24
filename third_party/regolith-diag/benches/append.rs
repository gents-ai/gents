//! Commit-ordered append and allocation bench.
//!
//! `append` rows time begin + append + commit under 1, 4 and 8 writers, with
//! and without a once key, beside the same shape with a plain put so the cost
//! of numbering is a difference between two rows of one run. `allocate` rows
//! time a single caller's latency (median and 99th percentile) and the rate of
//! 8 callers sharing one counter. Every figure is a median over repetitions,
//! reported with the observed spread; durability is the database default
//! (Eventual), so the numbers are CPU and pipeline cost, not fsync cost.

mod common;

use std::sync::Arc;
use std::time::Instant;

use regolith::{LogLayout, OptimisticTransactionDb, TxnOptions};

const VALUE_LEN: usize = 100;
const WRITERS: [usize; 3] = [1, 4, 8];

struct Journal;

impl LogLayout for Journal {
    fn head_key(&self) -> &[u8] {
        b"journal-head"
    }

    fn entry_key(&self, position: u64, out: &mut Vec<u8>) {
        out.extend_from_slice(format!("journal/{position:020}").as_bytes());
    }

    fn max_entry_key_len(&self) -> usize {
        8 + 20
    }
}

#[derive(Clone, Copy)]
enum Shape {
    Put,
    Append,
    AppendOnce,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Put => "put",
            Shape::Append => "append",
            Shape::AppendOnce => "append_once",
        }
    }
}

/// Commits per second of `threads` writers doing `per_thread` commits of
/// `shape` each, on a fresh database.
fn commit_rate(shape: Shape, threads: usize, per_thread: u64) -> f64 {
    let tmp = common::TempDb::new(&format!("append-{}-{threads}", shape.name()));
    let db = OptimisticTransactionDb::open(tmp.path(), common::default_opts())
        .unwrap_or_else(|e| panic!("open: {e}"));
    let log: Arc<dyn LogLayout> = Arc::new(Journal);
    let mut rng = common::Rng::new(0xA11C_0001);
    let value = common::rand_value(&mut rng, VALUE_LEN);
    let secs = common::start::on_threads(threads, threads as u64 * per_thread, |t, i| {
        let t = t as u64;
        let id = t * per_thread + i;
        let tx = db.begin(&TxnOptions::new());
        match shape {
            Shape::Put => tx.put(&common::key(id), &value).unwrap(),
            Shape::Append => tx.append(&log, &value, None).unwrap(),
            Shape::AppendOnce => {
                tx.append(&log, &value, Some(&common::key(id))).unwrap();
            }
        }
        tx.commit().unwrap_or_else(|e| panic!("commit: {e}"));
    })
    .as_secs_f64();
    assert!(secs > 0.0, "timed section reported a zero duration");
    (threads as u64 * per_thread) as f64 / secs
}

fn spread(samples: &mut Vec<f64>) -> String {
    let (lo, hi) = common::min_max(samples);
    format!(
        "{{\"median\":{:.1},\"min\":{lo:.1},\"max\":{hi:.1}}}",
        common::median(samples)
    )
}

/// Single caller's latency in nanoseconds at the median and the 99th
/// percentile, over `calls` allocations of 4 values.
fn allocate_latency(calls: usize) -> (f64, f64) {
    let (_tmp, db) = common::open("allocate-latency", common::default_opts());
    let mut nanos: Vec<f64> = Vec::with_capacity(calls);
    for _ in 0..calls {
        let start = Instant::now();
        db.allocate(b"ids", 4).unwrap();
        nanos.push(start.elapsed().as_nanos() as f64);
    }
    nanos.sort_by(|a, b| a.total_cmp(b));
    (nanos[calls / 2], nanos[(calls * 99 / 100).min(calls - 1)])
}

/// Allocations per second of `threads` callers on one counter.
fn allocate_rate(threads: usize, per_thread: u64) -> f64 {
    let (_tmp, db) = common::open("allocate-rate", common::default_opts());
    let secs = common::start::on_threads(threads, threads as u64 * per_thread, |_, _| {
        db.allocate(b"ids", 4).unwrap();
    })
    .as_secs_f64();
    assert!(secs > 0.0, "timed section reported a zero duration");
    (threads as u64 * per_thread) as f64 / secs
}

fn main() {
    let quick = std::env::args().any(|a| a == "--quick" || a == "--test");
    let (reps, per_thread, calls) = if quick {
        (2, 500, 2_000)
    } else {
        (7, 5_000, 50_000)
    };

    println!("append bench: begin + append + commit against begin + put + commit");
    let mut rows = Vec::new();
    for threads in WRITERS {
        let mut cells = Vec::new();
        for shape in [Shape::Put, Shape::Append, Shape::AppendOnce] {
            let mut samples: Vec<f64> = (0..reps)
                .map(|_| commit_rate(shape, threads, per_thread / threads as u64))
                .collect();
            let json = spread(&mut samples);
            println!(
                "{:<12} x{threads} writers: median {:.0} commits/s",
                shape.name(),
                common::median(&mut samples)
            );
            cells.push(format!("\"{}\":{json}", shape.name()));
        }
        rows.push(format!(
            "{{\"writers\":{threads},\"commits_per_s\":{{{}}}}}",
            cells.join(",")
        ));
    }

    let mut medians = Vec::with_capacity(reps);
    let mut tails = Vec::with_capacity(reps);
    for _ in 0..reps {
        let (p50, p99) = allocate_latency(calls);
        medians.push(p50);
        tails.push(p99);
    }
    let mut rates: Vec<f64> = (0..reps)
        .map(|_| allocate_rate(8, per_thread / 8))
        .collect();
    println!(
        "allocate: p50 {:.0} ns, p99 {:.0} ns single caller; {:.0} allocations/s from 8 callers",
        common::median(&mut medians.clone()),
        common::median(&mut tails.clone()),
        common::median(&mut rates.clone()),
    );

    common::write_family(
        "append",
        &format!(
            "{{\"quick\":{quick},\"reps\":{reps},\"durability\":\"eventual\",\"append\":[{}],\
             \"allocate\":{{\"latency_ns_p50\":{},\"latency_ns_p99\":{},\
             \"allocations_per_s_8_callers\":{}}}}}",
            rows.join(","),
            spread(&mut medians),
            spread(&mut tails),
            spread(&mut rates),
        ),
    );
}
