//! Point-read benchmarks for `Db::get`.
//!
//! `point_read` contrasts a block cache that holds the whole working set
//! against one that is far too small. Both variants are filled, compacted,
//! and then swept once with a full read pass, so the OS page cache is warm
//! in both and the only difference left is the block cache hit rate.
//!
//! `point_read_threads` measures aggregate read throughput at 1/2/4/8
//! threads and the CPU seconds burned per million reads at each width. CPU
//! accounting is process-wide and Linux-only; elsewhere it reports as
//! unavailable rather than as a measured zero.

mod common;

use std::cell::Cell;
use std::hint::black_box;
use std::thread;
use std::time::{Duration, Instant};

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, Options, WriteBatch};

const N_KEYS: u64 = 200_000;
const VALUE_LEN: usize = 200;
const HOT_CACHE: usize = 256 * 1024 * 1024;
const COLD_CACHE: usize = 1024 * 1024;
const FILL_BATCH: u64 = 1_000;
/// Reads per thread per criterion iteration. The threads are spawned before
/// the clock starts, so this sizes an iteration, not a spawn's share of it.
const OPS_PER_ITER: u64 = 10_000;
const THREAD_COUNTS: [usize; 4] = [1, 2, 4, 8];

/// Wall, CPU, and op counts summed over every iteration criterion drove,
/// warm-up included. Criterion reports its own estimate; this is the raw
/// total behind the JSON family, so the two can be compared.
#[derive(Default)]
struct Acc {
    ops: Cell<u64>,
    wall: Cell<f64>,
    cpu: Cell<f64>,
}

impl Acc {
    fn add(&self, ops: u64, wall: Duration, cpu: f64) {
        self.ops.set(self.ops.get() + ops);
        self.wall.set(self.wall.get() + wall.as_secs_f64());
        self.cpu.set(self.cpu.get() + cpu);
    }

    fn ops_per_s(&self) -> f64 {
        self.ops.get() as f64 / self.wall.get()
    }

    fn cpu_s_per_mop(&self) -> f64 {
        self.cpu.get() / (self.ops.get() as f64 / 1e6)
    }
}

/// JSON has no NaN or Infinity literal, so a degenerate sample emits null
/// instead of a plausible-looking number.
fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.3}")
    } else {
        "null".to_string()
    }
}

/// Fill, compact, then sweep. Without the compaction the whole dataset stays
/// resident in the memtable, the SSTable read path is never exercised, and
/// the hot/cold split measures nothing.
fn build(tag: &str, block_cache_size: usize, keys: &[Vec<u8>]) -> (common::TempDb, Db) {
    let opts = Options::default()
        .write_buffer_size(8 * 1024 * 1024)
        .block_cache_size(block_cache_size);
    let (tmp, db) = common::open(tag, opts);
    let mut rng = common::Rng::new(0x5EED_5EED);
    let mut i = 0u64;
    while i < N_KEYS {
        let end = (i + FILL_BATCH).min(N_KEYS);
        let mut batch = WriteBatch::new();
        while i < end {
            batch.put(&keys[i as usize], &common::rand_value(&mut rng, VALUE_LEN));
            i += 1;
        }
        db.write(batch).expect("fill write");
    }
    db.compact_range(None, None)
        .wait()
        .expect("fill compaction");
    for k in keys {
        let got = db.get(k).expect("warm read");
        assert!(got.is_some(), "key missing after fill");
    }
    (tmp, db)
}

/// Criterion's `--test` mode runs a single iteration per benchmark. Rates
/// derived from it are noise, so the run publishes no metric family.
fn is_test_run() -> bool {
    std::env::args().any(|a| a == "--test")
}

fn point_read(c: &mut Criterion) {
    let keys: Vec<Vec<u8>> = (0..N_KEYS).map(common::key).collect();
    let (_hot_dir, hot) = build("point-read-hot", HOT_CACHE, &keys);
    let (_cold_dir, cold) = build("point-read-cold", COLD_CACHE, &keys);

    let hot_acc = Acc::default();
    let cold_acc = Acc::default();

    let mut group = c.benchmark_group("point_read");
    group.throughput(Throughput::Elements(1));
    for (id, db, acc) in [
        ("hot_cache", &hot, &hot_acc),
        ("cold_cache", &cold, &cold_acc),
    ] {
        // Outside the closure, which criterion calls once per sample: a
        // sequence started again in each sample would find in the cache the
        // blocks the last sample read, and the cold cache would read warm.
        let mut rng = common::Rng::new(0xA5A5_1234);
        group.bench_function(id, |b| {
            b.iter_custom(|iters| {
                let cpu0 = common::cpu_seconds();
                let start = Instant::now();
                for _ in 0..iters {
                    let k = &keys[(rng.next() % N_KEYS) as usize];
                    black_box(db.get(k).expect("get"));
                }
                let wall = start.elapsed();
                acc.add(iters, wall, common::cpu_seconds() - cpu0);
                wall
            });
        });
    }
    group.finish();

    let thread_accs: Vec<Acc> = THREAD_COUNTS.iter().map(|_| Acc::default()).collect();
    let mut group = c.benchmark_group("point_read_threads");
    group.warm_up_time(Duration::from_secs(2));
    group.measurement_time(Duration::from_secs(8));
    group.sample_size(20);
    for (idx, &threads) in THREAD_COUNTS.iter().enumerate() {
        let acc = &thread_accs[idx];
        let db = &hot;
        let keys = &keys;
        group.throughput(Throughput::Elements(OPS_PER_ITER * threads as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let per_thread = iters * OPS_PER_ITER;
                    let line = common::start::StartLine::new(threads);
                    // The scope returns once every reader is done.
                    let (start, cpu0) = thread::scope(|s| {
                        for t in 0..threads {
                            let line = &line;
                            s.spawn(move || {
                                let mut rng =
                                    common::Rng::new(0x1234_5678 ^ ((t as u64) << 40) ^ iters);
                                line.wait();
                                for _ in 0..per_thread {
                                    let k = &keys[(rng.next() % N_KEYS) as usize];
                                    black_box(db.get(k).expect("get"));
                                }
                            });
                        }
                        line.all_ready();
                        let cpu0 = common::cpu_seconds();
                        (line.go(), cpu0)
                    });
                    let wall = start.elapsed();
                    let cpu = common::cpu_seconds() - cpu0;
                    let ops = per_thread * threads as u64;
                    acc.add(ops, wall, cpu);
                    wall
                });
            },
        );
    }
    group.finish();

    if is_test_run() {
        return;
    }

    let mut rows = String::new();
    for (idx, &threads) in THREAD_COUNTS.iter().enumerate() {
        let a = &thread_accs[idx];
        if idx > 0 {
            rows.push(',');
        }
        rows.push_str(&format!(
            "{{\"threads\":{threads},\"ops\":{},\"wall_s\":{},\"cpu_s\":{},\"ops_per_s\":{},\"cpu_s_per_mop\":{}}}",
            a.ops.get(),
            num(a.wall.get()),
            num(a.cpu.get()),
            num(a.ops_per_s()),
            num(a.cpu_s_per_mop()),
        ));
    }
    let first = thread_accs[0].ops_per_s();
    let last = thread_accs[THREAD_COUNTS.len() - 1].ops_per_s();
    common::write_family(
        "point_read",
        &format!(
            "{{\"keys\":{N_KEYS},\"value_bytes\":{VALUE_LEN},\
             \"cpu_accounting_available\":{},\
             \"hot_cache_ops_per_s\":{},\"cold_cache_ops_per_s\":{},\
             \"threads\":[{rows}],\"scaling_1_to_{}\":{}}}",
            cfg!(target_os = "linux"),
            num(hot_acc.ops_per_s()),
            num(cold_acc.ops_per_s()),
            THREAD_COUNTS[THREAD_COUNTS.len() - 1],
            num(last / first),
        ),
    );
}

criterion_group!(benches, point_read);
criterion_main!(benches);
