//! Per-core contention on the read path's shared structures (plan 4.6,
//! Phase 7b, D54): the read view, the block cache and a memtable's range
//! tombstones, at 1, 4, 16 and 64 threads.
//!
//! * `get_under_publisher`: point gets on tables while one writer keeps
//!   rotating small memtables, so the read view is republished under the
//!   readers the whole time. Every get loads the view.
//! * `miss_under_eviction`: point gets over a working set many times the
//!   block cache, so most reads miss and insert, and the CLOCK hand runs on
//!   every shard under every thread.
//! * `scan_with_tombstones`: short scans from random keys through a
//!   memtable holding range tombstones, each scan building an iterator that
//!   reads them, while a writer keeps appending more.
//!
//! Each row is reads per second summed over the threads, the median of the
//! repetitions with the spread beside it. `--quick` (or `--test`) shrinks
//! every count so the bench doubles as a smoke test.
//!
//! Run with `cargo bench --bench per_core_reads`. The snapshot registry and
//! the statistics shards, the other per-core structures, are in `per_core`.

mod common;

use std::sync::atomic::{AtomicBool, Ordering};

use regolith::{Db, Options, WriteBatch};

const THREADS: [usize; 4] = [1, 4, 16, 64];
const VALUE_LEN: usize = 100;

fn spread(samples: &mut Vec<f64>) -> String {
    let (lo, hi) = common::min_max(samples);
    format!(
        "{{\"median\":{:.1},\"min\":{lo:.1},\"max\":{hi:.1}}}",
        common::median(samples)
    )
}

/// Write `keys` keys and compact them into tables.
fn fill(db: &Db, keys: u64) {
    let mut rng = common::Rng::new(0x7B_0001);
    let mut i = 0u64;
    while i < keys {
        let end = (i + 1_000).min(keys);
        let mut batch = WriteBatch::new();
        while i < end {
            batch.put(&common::key(i), &common::rand_value(&mut rng, VALUE_LEN));
            i += 1;
        }
        db.write(batch).expect("fill write");
    }
    db.compact_range(None, None)
        .wait()
        .expect("fill compaction");
}

/// Reads per second of `threads` threads each running `read` `per_thread`
/// times, while `background` runs on one more thread until they finish.
fn rate(
    threads: usize,
    per_thread: u64,
    read: impl Fn(&mut common::Rng) + Sync,
    background: impl Fn(&AtomicBool) + Sync,
) -> f64 {
    let done = AtomicBool::new(false);
    let line = common::start::StartLine::new(threads);
    let elapsed = std::thread::scope(|scope| {
        let background = &background;
        let done_ref = &done;
        let writer = scope.spawn(move || background(done_ref));
        let readers: Vec<_> = (0..threads as u64)
            .map(|t| {
                let (read, line) = (&read, &line);
                scope.spawn(move || {
                    let mut rng = common::Rng::new(0x7B_1000 + t);
                    line.wait();
                    for _ in 0..per_thread {
                        read(&mut rng);
                    }
                })
            })
            .collect();
        let start = line.start();
        for reader in readers {
            reader.join().expect("reader");
        }
        let elapsed = start.elapsed();
        done.store(true, Ordering::Release);
        writer.join().expect("background");
        elapsed
    });
    (threads as u64 * per_thread) as f64 / elapsed.as_secs_f64()
}

fn get_under_publisher(threads: usize, keys: u64, per_thread: u64) -> f64 {
    let (_tmp, db) = common::open(
        &format!("per-core-view-{threads}"),
        Options::default().write_buffer_size(64 * 1024),
    );
    fill(&db, keys);
    rate(
        threads,
        per_thread,
        |rng| {
            let key = common::key(rng.next() % keys);
            std::hint::black_box(db.get(&key).expect("get"));
        },
        |done| {
            let mut rng = common::Rng::new(0x7B_2000);
            let mut i = keys;
            while !done.load(Ordering::Acquire) {
                db.put(&common::key(i), &common::rand_value(&mut rng, VALUE_LEN))
                    .expect("publisher write");
                i += 1;
            }
        },
    )
}

fn miss_under_eviction(threads: usize, keys: u64, per_thread: u64) -> f64 {
    let (_tmp, db) = common::open(
        &format!("per-core-cache-{threads}"),
        Options::default()
            .block_size(1024)
            .block_cache_size(1024 * 1024)
            .block_cache_num_shard_bits(4),
    );
    fill(&db, keys);
    rate(
        threads,
        per_thread,
        |rng| {
            let key = common::key(rng.next() % keys);
            std::hint::black_box(db.get(&key).expect("get"));
        },
        |_| {},
    )
}

fn scan_with_tombstones(threads: usize, keys: u64, per_thread: u64) -> f64 {
    let (_tmp, db) = common::open(
        &format!("per-core-tombstones-{threads}"),
        Options::default().write_buffer_size(256 * 1024 * 1024),
    );
    let mut rng = common::Rng::new(0x7B_3000);
    for i in 0..keys {
        db.put(&common::key(i), &common::rand_value(&mut rng, VALUE_LEN))
            .expect("fill");
    }
    // Every tenth range of ten keys deleted, all in the active memtable.
    for i in (0..keys).step_by(100) {
        db.delete_range(&common::key(i), &common::key(i + 10))
            .expect("range delete");
    }
    rate(
        threads,
        per_thread,
        |rng| {
            let start = common::key(rng.next() % keys);
            std::hint::black_box(db.scan_page(Some(&start), None, 16).expect("scan"));
        },
        |done| {
            let mut i = keys;
            while !done.load(Ordering::Acquire) {
                db.delete_range(&common::key(i), &common::key(i + 1))
                    .expect("background range delete");
                i += 2;
            }
        },
    )
}

fn main() {
    let quick = std::env::args().any(|a| a == "--quick" || a == "--test");
    let (reps, keys, reads) = if quick {
        (1, 2_000, 2_000)
    } else {
        (5, 200_000, 400_000)
    };

    type Case = fn(usize, u64, u64) -> f64;
    let cases: [(&str, Case); 3] = [
        ("get_under_publisher", get_under_publisher),
        ("miss_under_eviction", miss_under_eviction),
        ("scan_with_tombstones", scan_with_tombstones),
    ];
    let mut families = Vec::new();
    for (name, case) in cases {
        let mut rows = Vec::new();
        for threads in THREADS {
            let per_thread = (reads / threads as u64).max(1);
            let mut samples: Vec<f64> =
                (0..reps).map(|_| case(threads, keys, per_thread)).collect();
            println!(
                "{name:<22} x{threads:<2} threads: median {:.0} reads/s",
                common::median(&mut samples)
            );
            rows.push(format!(
                "{{\"threads\":{threads},\"reads_per_s\":{}}}",
                spread(&mut samples)
            ));
        }
        families.push(format!("\"{name}\":[{}]", rows.join(",")));
    }
    common::write_family(
        "per_core_reads",
        &format!("{{\"quick\":{quick},{}}}", families.join(",")),
    );
}
