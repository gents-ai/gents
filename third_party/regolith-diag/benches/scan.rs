//! Iteration benchmarks: one full streaming pass over the whole database,
//! plus forward seek, reverse seek, and prefix seek.
//!
//! The working set is 256 MiB of values rather than the 2 GiB a production
//! scan would face. At 2 GiB the fill and the compaction that follows it
//! dominate the wall clock of the run; 256 MiB against a 64 MiB block cache
//! already puts the scan in the regime a 2 GiB scan runs in, where the
//! stream cannot be served from cache. The size is in the benchmark id so a
//! reported MiB/s is never read as a 2 GiB result.

mod common;

use std::cell::Cell;
use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, Options, WriteBatch, WriteOptions};

const VALUE_LEN: usize = 1024;
/// 256 MiB of value bytes.
const N_KEYS: u64 = 262_144;
const BLOCK_CACHE: usize = 64 * 1024 * 1024;
const FILL_BATCH: u64 = 256;
/// Keys sharing an 11-digit prefix: `key(i)` is 12 zero-padded digits, so
/// dropping the last one groups exactly ten keys.
const PREFIX_GROUP: u64 = 10;

fn key_bytes() -> u64 {
    common::key(0).len() as u64
}

/// Logical bytes the iterator hands back over a full pass. Values are random,
/// so lz4 leaves them near their raw size and this tracks the bytes actually
/// read off disk closely.
fn scanned_bytes() -> u64 {
    N_KEYS * (key_bytes() + VALUE_LEN as u64)
}

fn num(x: f64) -> String {
    if x.is_finite() {
        format!("{x:.3}")
    } else {
        "null".to_string()
    }
}

/// The fill skips the WAL: it is setup, not a measured write path, and the
/// compaction that follows puts every key in an SSTable anyway.
fn build(keys: &[Vec<u8>]) -> (common::TempDb, Db) {
    let opts = Options::default().block_cache_size(BLOCK_CACHE);
    let (tmp, db) = common::open("scan", opts);
    let wopts = WriteOptions {
        disable_wal: true,
        ..WriteOptions::default()
    };
    let mut rng = common::Rng::new(0x5CA4_5CA4);
    let mut i = 0u64;
    while i < N_KEYS {
        let end = (i + FILL_BATCH).min(N_KEYS);
        let mut batch = WriteBatch::new();
        while i < end {
            batch.put(&keys[i as usize], &common::rand_value(&mut rng, VALUE_LEN));
            i += 1;
        }
        db.write_opt(&wopts, batch).expect("fill write");
    }
    db.compact_range(None, None)
        .wait()
        .expect("fill compaction");
    (tmp, db)
}

/// Criterion's `--test` mode runs a single iteration per benchmark. Rates
/// derived from it are noise, so the run publishes no metric family.
fn is_test_run() -> bool {
    std::env::args().any(|a| a == "--test")
}

fn scan(c: &mut Criterion) {
    let keys: Vec<Vec<u8>> = (0..N_KEYS).map(common::key).collect();
    let (_dir, db) = build(&keys);

    let bytes = Cell::new(0u64);
    let wall = Cell::new(0f64);

    let mut group = c.benchmark_group("scan");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(20));
    group.sample_size(10);
    group.throughput(Throughput::Bytes(scanned_bytes()));
    group.bench_function("stream_full_256MiB", |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            let mut seen = 0u64;
            let mut read = 0u64;
            for _ in 0..iters {
                let mut it = db.iter();
                it.seek_to_first();
                while it.valid() {
                    let k = it.key().expect("valid iterator has a key");
                    let v = it.value().expect("valid iterator has a value");
                    read += (k.len() + v.len()) as u64;
                    seen += 1;
                    it.next();
                }
                it.status().expect("scan status");
            }
            let elapsed = start.elapsed();
            // A truncated scan would otherwise report as a very fast one.
            assert_eq!(seen, iters * N_KEYS, "full scan missed keys");
            bytes.set(bytes.get() + read);
            wall.set(wall.get() + elapsed.as_secs_f64());
            black_box(read);
            elapsed
        });
    });
    group.finish();

    let mut group = c.benchmark_group("scan_seek");
    group.throughput(Throughput::Elements(1));
    // Each key sequence lives outside its closure, which criterion calls once
    // per sample: started again in each sample, it would find in the cache
    // the blocks the last sample read.

    let mut rng = common::Rng::new(0x1111_2222);
    group.bench_function("seek", |b| {
        b.iter(|| {
            let target = &keys[(rng.next() % N_KEYS) as usize];
            let mut it = db.iter();
            it.seek(target);
            assert!(it.valid(), "seek landed outside the keyspace");
            black_box(it.key().map(|k| k.len()))
        });
    });

    let mut rng = common::Rng::new(0x3333_4444);
    group.bench_function("seek_for_prev", |b| {
        b.iter(|| {
            let target = &keys[(rng.next() % N_KEYS) as usize];
            let mut it = db.iter();
            it.seek_for_prev(target);
            assert!(it.valid(), "seek_for_prev landed outside the keyspace");
            black_box(it.key().map(|k| k.len()))
        });
    });

    // Seek plus a drain of the whole ten-key prefix group, which is what a
    // prefix lookup actually costs a caller.
    let mut rng = common::Rng::new(0x5555_6666);
    group.bench_function("seek_prefix_group_of_10", |b| {
        let groups = N_KEYS / PREFIX_GROUP;
        b.iter(|| {
            let g = rng.next() % groups;
            let prefix = format!("key{g:011}").into_bytes();
            let mut it = db.iter();
            it.seek_prefix(&prefix);
            let mut seen = 0u64;
            while it.valid() {
                seen += 1;
                it.next();
            }
            assert_eq!(seen, PREFIX_GROUP, "prefix seek missed keys in the group");
            black_box(seen)
        });
    });
    group.finish();

    if is_test_run() {
        return;
    }

    let mib_per_s = (bytes.get() as f64 / (1024.0 * 1024.0)) / wall.get();
    common::write_family(
        "scan",
        &format!(
            "{{\"keys\":{N_KEYS},\"value_bytes\":{VALUE_LEN},\
             \"working_set_bytes\":{},\"scanned_bytes\":{},\"wall_s\":{},\
             \"stream_mib_per_s\":{}}}",
            scanned_bytes(),
            bytes.get(),
            num(wall.get()),
            num(mib_per_s),
        ),
    );
}

criterion_group!(benches, scan);
criterion_main!(benches);
