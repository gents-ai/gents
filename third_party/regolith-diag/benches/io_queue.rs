//! Benchmarks for non-blocking reads on per-thread I/O queues (plan 4.10,
//! D53).
//!
//! `io_queue_hit` reads blocks the cache holds, through a `Blocking` and a
//! `CacheOnly` snapshot: the price of the mode on a hit, which should be the
//! two thread-local writes that set and clear it.
//!
//! `io_queue_miss_to_ready` reads blocks the cache never holds (it is
//! disabled) and compares the two ways a thread that must not block can get
//! a block from the device: through its own queue (the read returns
//! `WouldBlock`, the thread polls its queue, which reads the block, and runs
//! the read again) against a hop to an I/O thread (the read is handed over a
//! channel to a thread that reads blocking, and the answer comes back the
//! same way). The first is D53's design; the second is the design it
//! replaced. A queue keeps the blocks it read for the reads run again, and a
//! plain read keeps nothing, so a key order that came back to a block the
//! queue still held would time a device read on one side and none on the
//! other: both sides read the keys in one [`Sweep`], which reads a block
//! again only long after the queue let it go. The table reads each side
//! made per get are counted and printed, and must agree.
//!
//! The `Blocking` read path itself is measured before and after by
//! `point_read` and `scan`.

mod common;

use std::cell::Cell;
use std::hint::black_box;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, Error, IoBudget, Options, ReadMode, WouldBlock, WriteBatch};

const N_KEYS: u64 = 100_000;
/// The miss rows' keys: enough that a [`Sweep`] pass reads several times as
/// many blocks as a queue keeps landed.
const MISS_KEYS: u64 = 400_000;
const VALUE_LEN: usize = 100;
const HOT_CACHE: usize = 256 * 1024 * 1024;
const FILL_BATCH: u64 = 1_000;

/// Fill, compact into tables, then read every key once so the OS page
/// cache holds the files and a block read costs a copy, not a disk seek.
fn build(tag: &str, opts: Options, keys: &[Vec<u8>]) -> (common::TempDb, Db) {
    let opts = opts.write_buffer_size(8 * 1024 * 1024);
    let (tmp, db) = common::open(tag, opts);
    let mut rng = common::Rng::new(0x10_0E0E);
    let n = keys.len() as u64;
    let mut i = 0u64;
    while i < n {
        let end = (i + FILL_BATCH).min(n);
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
        assert!(db.get(k).expect("warm read").is_some(), "key missing");
    }
    (tmp, db)
}

/// The order the miss rows read keys in. A pass steps `stride` keys at a
/// time through the keyspace and the next pass starts one key further on.
/// Every entry holds at least `VALUE_LEN` bytes, so a block holds at most
/// `block_size / VALUE_LEN` keys and two keys `stride`, twice that, apart are
/// never in one block: a pass reads distinct blocks, and a block is read
/// again only a pass later, after every other block of the pass.
struct Sweep {
    keys: u64,
    stride: u64,
    offset: u64,
    at: u64,
}

impl Sweep {
    fn new(keys: u64, block_size: usize) -> Self {
        Sweep {
            keys,
            stride: 2 * block_size.div_ceil(VALUE_LEN) as u64,
            offset: 0,
            at: 0,
        }
    }

    fn next_key(&mut self) -> usize {
        let at = self.at;
        self.at += self.stride;
        if self.at >= self.keys {
            self.offset = (self.offset + 1) % self.stride;
            self.at = self.offset;
        }
        at as usize
    }
}

/// Gets and table reads one side of the miss comparison made, over every
/// iteration criterion ran, warm-up included.
#[derive(Default)]
struct Reads {
    gets: Cell<u64>,
    table_reads: Cell<u64>,
}

impl Reads {
    fn add(&self, gets: u64, table_reads: u64) {
        self.gets.set(self.gets.get() + gets);
        self.table_reads.set(self.table_reads.get() + table_reads);
    }

    fn per_get(&self) -> f64 {
        self.table_reads.get() as f64 / self.gets.get() as f64
    }
}

fn io_queue_hit(c: &mut Criterion) {
    let keys: Vec<Vec<u8>> = (0..N_KEYS).map(common::key).collect();
    let opts = Options::default().block_cache_size(HOT_CACHE);
    let (_dir, db) = build("io-queue-hit", opts, &keys);
    let queue = db.io_queue();
    let blocking = db.snapshot();
    let cache_only = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));

    let mut group = c.benchmark_group("io_queue_hit");
    group.throughput(Throughput::Elements(1));
    for (id, snapshot) in [("blocking", &blocking), ("cache_only", &cache_only)] {
        group.bench_function(id, |b| {
            let mut rng = common::Rng::new(0x41_7E57);
            b.iter(|| {
                let k = &keys[(rng.next() % N_KEYS) as usize];
                black_box(snapshot.get(k).expect("a cached read"))
            });
        });
    }
    group.finish();
}

fn io_queue_miss_to_ready(c: &mut Criterion) {
    let keys: Vec<Vec<u8>> = (0..MISS_KEYS).map(common::key).collect();
    let env = common::counting_env::CountingEnv::new();
    let opts = Options::default().block_cache_size(0).env(env.clone());
    let block_size = opts.get_block_size();
    let (_dir, db) = build("io-queue-miss", opts, &keys);

    let mut group = c.benchmark_group("io_queue_miss_to_ready");
    group.throughput(Throughput::Elements(1));

    let own = Reads::default();
    let mut queue = db.io_queue();
    let snapshot = db
        .snapshot()
        .with_read_mode(ReadMode::CacheOnly(queue.id()));
    // Criterion calls the closure once per sample, so the sweep lives outside
    // it: a sweep started again in each sample would read again the blocks
    // the last sample read, which the queue still holds.
    let mut sweep = Sweep::new(MISS_KEYS, block_size);
    group.bench_function("own_queue", |b| {
        b.iter_custom(|iters| {
            let reads = env.table_reads();
            let start = Instant::now();
            for _ in 0..iters {
                let k = &keys[sweep.next_key()];
                let found = loop {
                    match snapshot.get(k) {
                        Err(Error::WouldBlock(WouldBlock::Io(_))) => {
                            queue.poll(IoBudget::ALL);
                        }
                        other => break other.expect("read"),
                    }
                };
                black_box(found);
            }
            let elapsed = start.elapsed();
            own.add(iters, env.table_reads() - reads);
            elapsed
        });
    });

    let hop = Reads::default();
    let (requests, served) = mpsc::channel::<&[u8]>();
    let (answers, answered) = mpsc::channel::<Option<Vec<u8>>>();
    thread::scope(|scope| {
        let db = &db;
        scope.spawn(move || {
            for k in served {
                answers.send(db.get(k).expect("read")).expect("answer");
            }
        });
        let mut sweep = Sweep::new(MISS_KEYS, block_size);
        group.bench_function("hop_to_io_thread", |b| {
            b.iter_custom(|iters| {
                let reads = env.table_reads();
                let start = Instant::now();
                for _ in 0..iters {
                    requests.send(&keys[sweep.next_key()]).expect("request");
                    black_box(answered.recv().expect("answer"));
                }
                let elapsed = start.elapsed();
                hop.add(iters, env.table_reads() - reads);
                elapsed
            });
        });
        drop(requests);
    });
    group.finish();

    println!(
        "io_queue_miss_to_ready table reads per get: own_queue {:.3} ({} reads, {} gets), \
         hop_to_io_thread {:.3} ({} reads, {} gets)",
        own.per_get(),
        own.table_reads.get(),
        own.gets.get(),
        hop.per_get(),
        hop.table_reads.get(),
        hop.gets.get(),
    );
    // Equal reads per get, and at least one: otherwise the two sides timed
    // different work, and the comparison above says nothing.
    assert!(
        own.per_get() >= 1.0 && (own.per_get() - hop.per_get()).abs() < 0.01,
        "the two sides of io_queue_miss_to_ready read the device a different number of times"
    );
}

criterion_group!(benches, io_queue_hit, io_queue_miss_to_ready);
criterion_main!(benches);
