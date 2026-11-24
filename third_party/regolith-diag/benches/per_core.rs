//! The per-core hot paths under contention: snapshot registration and
//! statistics tickers (plan 4.6, D54).
//!
//! Run with `cargo bench --bench per_core`. The read path's per-core
//! structures (the read view, the block cache, range tombstones) are in
//! `per_core_reads`.
//!
//! Each case runs on 1, 4, 16 and 64 threads at once against one database;
//! one element is one operation on one thread, and the reported time is the
//! wall time from a common start until every thread is done.
//!
//! - **`snapshot_begin_end`**: `Db::snapshot()` then drop. With statistics
//!   off it is the snapshot registry alone: a pin in the thread's own slot
//!   and its release. With statistics on, each snapshot also adds two
//!   tickers (`regolith.snapshot.registered` and `.released`) to the
//!   thread's statistics shard; the difference between the two rows is the
//!   ticker cost.
//! - **`txn_begin_rollback`**: an optimistic transaction begun and dropped:
//!   the registration a transaction pays.
//! - **`get`**: a point read of a memtable key with statistics off and on;
//!   on, every read adds two tickers and two histogram samples.
//!
//! Under the old design every thread took one mutex for each snapshot and
//! incremented one shared ticker array, so these rows fall with threads;
//! with per-thread slots and shards they should stay flat up to the core
//! count. No numbers are recorded here: plan 7.4 holds measured results.

mod common;

use std::hint::black_box;
use std::sync::Arc;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, OptimisticTransactionDb, Options, Statistics, TxnOptions};

const THREADS: [usize; 4] = [1, 4, 16, 64];

fn options(statistics: bool) -> Options {
    let opts = Options::default().max_background_compactions(0);
    if statistics {
        opts.statistics(Some(Arc::new(Statistics::new())))
    } else {
        opts
    }
}

fn snapshot_begin_end(c: &mut Criterion) {
    let mut group = c.benchmark_group("snapshot_begin_end");
    group.throughput(Throughput::Elements(1));
    for statistics in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(dir.path(), options(statistics)).expect("open");
        db.put(b"k", b"v").expect("put");
        let name = if statistics {
            "statistics_on"
        } else {
            "statistics_off"
        };
        for threads in THREADS {
            group.bench_with_input(BenchmarkId::new(name, threads), &threads, |b, &threads| {
                b.iter_custom(|iters| {
                    common::start::on_threads(threads, iters, |_, _| {
                        drop(black_box(db.snapshot()));
                    })
                })
            });
        }
    }
    group.finish();
}

fn txn_begin_rollback(c: &mut Criterion) {
    let mut group = c.benchmark_group("txn_begin_rollback");
    group.throughput(Throughput::Elements(1));
    let dir = tempfile::tempdir().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), options(false)).expect("open");
    for threads in THREADS {
        group.bench_with_input(
            BenchmarkId::from_parameter(threads),
            &threads,
            |b, &threads| {
                b.iter_custom(|iters| {
                    common::start::on_threads(threads, iters, |_, _| {
                        drop(black_box(db.begin(&TxnOptions::default())));
                    })
                })
            },
        );
    }
    group.finish();
}

fn get(c: &mut Criterion) {
    let mut group = c.benchmark_group("get");
    group.throughput(Throughput::Elements(1));
    for statistics in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = Db::open(dir.path(), options(statistics)).expect("open");
        db.put(b"k", b"v").expect("put");
        let name = if statistics {
            "statistics_on"
        } else {
            "statistics_off"
        };
        for threads in THREADS {
            group.bench_with_input(BenchmarkId::new(name, threads), &threads, |b, &threads| {
                b.iter_custom(|iters| {
                    common::start::on_threads(threads, iters, |_, _| {
                        black_box(db.get(b"k").expect("get"));
                    })
                })
            });
        }
    }
    group.finish();
}

criterion_group!(benches, snapshot_begin_end, txn_begin_rollback, get);
criterion_main!(benches);
