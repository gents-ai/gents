//! Contention on the leaf tables every read or file operation touches
//! (plan 4.6, Phase 7c1), at 1, 4, 16 and 64 threads.
//!
//! - `open_files`: point reads over many tables through a small
//!   `max_open_files`, so reads join, evict and reload descriptors in the
//!   lock-free slot table all the time. The block cache is off, so every
//!   read reaches a table's descriptor.
//! - `mem_env`: positional reads of `MemEnv` files while one writer per
//!   file keeps appending: the lock-free file maps and in-memory files.
//! - `column_families`: `put_cf` and `get_cf` spread over several column
//!   families: the registry lookups and the ordered step's fence.
//!
//! Each case runs a fixed number of operations on each thread from a
//! common start and reports operations per second.

mod common;

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use regolith::env::{Env, MemEnv, ReadFile, WriteMode};
use regolith::{ColumnFamilyHandle, Db, Options, WriteBatch};

/// The thread counts every case runs at.
const THREADS: [usize; 4] = [1, 4, 16, 64];

const TABLES: u64 = 64;
const KEYS_PER_TABLE: u64 = 500;

/// A store of `TABLES` level-0 tables read through `max_open_files`
/// descriptors, with no block cache.
fn open_files_db(max_open_files: usize) -> (common::TempDb, Db) {
    let opts = Options::default()
        .max_open_files(max_open_files)
        .block_cache_size(0)
        .l0_compaction_trigger(10_000)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0);
    let (tmp, db) = common::open("tables-open-files", opts);
    let mut rng = common::Rng::new(0x0F11E5);
    for t in 0..TABLES {
        let mut batch = WriteBatch::new();
        for k in 0..KEYS_PER_TABLE {
            batch.put(
                &common::key(t * KEYS_PER_TABLE + k),
                &common::rand_value(&mut rng, 64),
            );
        }
        db.write(batch).expect("fill");
        db.flush().expect("flush");
    }
    (tmp, db)
}

fn open_files(c: &mut Criterion) {
    let mut group = c.benchmark_group("tables_open_files");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));
    for max_open_files in [8usize, 32] {
        let (_dir, db) = open_files_db(max_open_files);
        let db = Arc::new(db);
        for threads in THREADS {
            let id = format!("get_max{max_open_files}");
            group.bench_with_input(BenchmarkId::new(id, threads), &threads, |b, &threads| {
                b.iter_custom(|iters| {
                    let db = Arc::clone(&db);
                    common::start::on_threads(threads, iters, move |t, i| {
                        let k = (i * 7919 + t as u64 * 104_729) % (TABLES * KEYS_PER_TABLE);
                        black_box(db.get(&common::key(k)).expect("read"));
                    })
                })
            });
        }
    }
    group.finish();
}

const FILES: usize = 16;
const FILE_BYTES: usize = 1 << 20;

/// `FILES` files of `FILE_BYTES` each in a fresh `MemEnv`.
fn mem_env_files() -> (MemEnv, Vec<PathBuf>) {
    let env = MemEnv::new();
    env.create_dir_all(Path::new("/bench")).expect("mkdir");
    let paths: Vec<PathBuf> = (0..FILES)
        .map(|i| PathBuf::from(format!("/bench/{i:04}.sst")))
        .collect();
    for path in &paths {
        env.write(path, &vec![7u8; FILE_BYTES]).expect("fill");
    }
    (env, paths)
}

fn mem_env(c: &mut Criterion) {
    let mut group = c.benchmark_group("tables_mem_env");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));
    let (env, paths) = mem_env_files();
    let files: Arc<Vec<Box<dyn ReadFile>>> = Arc::new(
        paths
            .iter()
            .map(|p| env.open_read(p).expect("open"))
            .collect(),
    );
    for threads in THREADS {
        group.bench_with_input(
            BenchmarkId::new("read_4k", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let files = Arc::clone(&files);
                    common::start::on_threads(threads, iters, move |t, i| {
                        let file = &files[(t + i as usize) % FILES];
                        let at = (i * 4096) % (FILE_BYTES as u64 - 4096);
                        let mut buf = [0u8; 4096];
                        file.read_exact_at(at, &mut buf).expect("read");
                        black_box(&buf);
                    })
                })
            },
        );
        // Every fourth thread appends to its own log while the rest read.
        group.bench_with_input(
            BenchmarkId::new("read_beside_appends", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let (env, files) = (env.clone(), Arc::clone(&files));
                    common::start::on_threads(threads, iters, move |t, i| {
                        if t % 4 == 3 {
                            let log = PathBuf::from(format!("/bench/log{t}"));
                            let mut file = env.open_write(&log, WriteMode::Append).expect("log");
                            file.write_all(&i.to_le_bytes()).expect("append");
                        } else {
                            let file = &files[(t + i as usize) % FILES];
                            let mut buf = [0u8; 512];
                            file.read_exact_at((i * 512) % (FILE_BYTES as u64 - 512), &mut buf)
                                .expect("read");
                            black_box(&buf);
                        }
                    })
                })
            },
        );
    }
    group.finish();
}

const FAMILIES: usize = 8;

fn column_families(c: &mut Criterion) {
    let mut group = c.benchmark_group("tables_column_families");
    group.sample_size(10);
    group.throughput(Throughput::Elements(1));
    let (_dir, db) = common::open("tables-cf", Options::default());
    let families: Arc<Vec<ColumnFamilyHandle>> = Arc::new(
        (0..FAMILIES)
            .map(|f| db.create_column_family(&format!("cf{f}")).expect("create"))
            .collect(),
    );
    let db = Arc::new(db);
    for family in families.iter() {
        for k in 0..1_000u64 {
            db.put_cf(family, &common::key(k), b"value").expect("fill");
        }
    }
    for threads in THREADS {
        group.bench_with_input(
            BenchmarkId::new("get_cf", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let (db, families) = (Arc::clone(&db), Arc::clone(&families));
                    common::start::on_threads(threads, iters, move |t, i| {
                        let family = &families[(t + i as usize) % FAMILIES];
                        black_box(db.get_cf(family, &common::key(i % 1_000)).expect("read"));
                    })
                })
            },
        );
        group.bench_with_input(
            BenchmarkId::new("put_cf", threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    let (db, families) = (Arc::clone(&db), Arc::clone(&families));
                    common::start::on_threads(threads, iters, move |t, i| {
                        let family = &families[(t + i as usize) % FAMILIES];
                        db.put_cf(family, &common::key(i % 1_000), b"value")
                            .expect("write");
                    })
                })
            },
        );
    }
    group.finish();
}

criterion_group!(benches, open_files, mem_env, column_families);
criterion_main!(benches);
