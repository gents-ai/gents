//! Streaming scan against materializing scan.
//!
//! [`Db::scan`] builds the whole range into a `Vec` before the caller sees a
//! row; [`Db::scan_stream`] hands rows over as it walks. On throughput alone
//! the two sit within noise of each other, which is why measuring only that
//! would report "no difference" for an API whose entire purpose is a
//! difference. What separates them is memory and latency:
//!
//! * **peak live bytes**, which is flat in the range for the stream and linear
//!   for the materializing pass. This is the number the API exists to change.
//! * **time to first row**, which the materializing pass cannot produce until
//!   it has built everything.
//! * **early-stop cost**, the shape most callers actually have: read a page and
//!   stop. The stream pays for the page, the materializing pass for the range.
//!
//! The working set is 32 MiB rather than the 256 MiB of `scan.rs`, because
//! this bench holds the whole range in memory on the materializing side and a
//! 256 MiB range would put half a gigabyte of `Vec` on a CI runner. 32 MiB is
//! already far past the point where the materializing cost is visibly linear.
//! The size is in every benchmark id so no number here is read as a 256 MiB
//! result.
//!
//! Row counts are asserted rather than assumed. A scan that silently returns
//! the wrong rows would otherwise report as a very fast one.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use regolith::{Db, Options, WriteBatch, WriteOptions};

const VALUE_LEN: usize = 1024;
/// 32 MiB of value bytes.
const N_KEYS: u64 = 32_768;
const BLOCK_CACHE: usize = 8 * 1024 * 1024;
const FILL_BATCH: u64 = 256;
/// A page, which is the shape an early-stopping caller has.
const PAGE: usize = 100;

/// Process-wide gate. Fill and teardown run with counting off, so only the
/// measured region is attributed.
static COUNTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

thread_local! {
    /// Const-initialized and `Drop`-free, so the allocator hook can touch it
    /// without lazy initialization allocating re-entrantly.
    static LIVE: Cell<i64> = const { Cell::new(0) };
    static PEAK: Cell<i64> = const { Cell::new(0) };
}

/// Counts *live* bytes rather than cumulative ones, because the question is
/// how much the caller has to hold at once, not how much it touched.
///
/// Thread-local, so background compaction on its own thread is excluded even
/// when it overlaps the measured region.
struct PeakAlloc;

fn counting() -> bool {
    COUNTING.load(std::sync::atomic::Ordering::Relaxed)
}

fn record(delta: i64) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + delta;
        live.set(now);
        let _ = PEAK.try_with(|peak| {
            if now > peak.get() {
                peak.set(now);
            }
        });
    });
}

unsafe impl GlobalAlloc for PeakAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && counting() {
            record(layout.size() as i64);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if counting() {
            record(-(layout.size() as i64));
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let out = unsafe { System.realloc(ptr, layout, new_size) };
        if !out.is_null() && counting() {
            record(new_size as i64 - layout.size() as i64);
        }
        out
    }
}

#[global_allocator]
static ALLOC: PeakAlloc = PeakAlloc;

/// Run `f` with counting on and report the high-water mark of live bytes.
fn peak_live_bytes<T>(f: impl FnOnce() -> T) -> (T, u64) {
    LIVE.with(|live| live.set(0));
    PEAK.with(|peak| peak.set(0));
    COUNTING.store(true, std::sync::atomic::Ordering::Relaxed);
    let out = f();
    COUNTING.store(false, std::sync::atomic::Ordering::Relaxed);
    let peak = PEAK.with(|peak| peak.get()).max(0) as u64;
    (out, peak)
}

fn key_bytes() -> u64 {
    common::key(0).len() as u64
}

fn scanned_bytes() -> u64 {
    N_KEYS * (key_bytes() + VALUE_LEN as u64)
}

fn num(x: f64) -> String {
    format!("{x:.3}")
}

fn build(keys: &[Vec<u8>]) -> (common::TempDb, Db) {
    let opts = Options::default().block_cache_size(BLOCK_CACHE);
    let (tmp, db) = common::open("scan_stream", opts);
    let wopts = WriteOptions {
        disable_wal: true,
        ..WriteOptions::default()
    };
    let mut rng = common::Rng::new(0x57BE_A115);
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

fn is_test_run() -> bool {
    std::env::args().any(|a| a == "--test")
}

/// Consume the whole range through the streaming API.
fn stream_all(db: &Db) -> u64 {
    let mut seen = 0u64;
    for entry in db.scan_stream(None, None).expect("scan_stream") {
        let (key, value) = entry.expect("scan row");
        black_box((key.len(), value.len()));
        seen += 1;
    }
    seen
}

/// Consume the whole range through the materializing API.
fn materialize_all(db: &Db) -> u64 {
    let rows = db.scan(None, None).expect("scan");
    let seen = rows.len() as u64;
    black_box(&rows);
    seen
}

fn scan_stream(c: &mut Criterion) {
    let keys: Vec<Vec<u8>> = (0..N_KEYS).map(common::key).collect();
    let (_dir, db) = build(&keys);

    // Correctness before speed: a scan that returns the wrong rows would
    // otherwise show up here as a fast one. This also pins the defect where a
    // cursor seeked past the end restarted at the first key.
    assert_eq!(stream_all(&db), N_KEYS, "streaming scan missed keys");
    assert_eq!(
        materialize_all(&db),
        N_KEYS,
        "materializing scan missed keys"
    );
    assert_eq!(
        db.scan_stream(Some(&common::key(N_KEYS + 1)), None)
            .expect("scan_stream past the end")
            .count(),
        0,
        "a scan starting past the last key must be empty"
    );

    let mut group = c.benchmark_group("scan_stream/full_32MiB");
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(10);
    group.throughput(Throughput::Bytes(scanned_bytes()));
    group.bench_function("stream", |b| b.iter(|| black_box(stream_all(&db))));
    group.bench_function("materialize", |b| {
        b.iter(|| black_box(materialize_all(&db)))
    });
    group.finish();

    // What the streaming API is for. The materializing pass cannot hand over a
    // row until it has built every row.
    let mut group = c.benchmark_group("scan_stream/first_row_32MiB");
    group.throughput(Throughput::Elements(1));
    group.bench_function("stream", |b| {
        b.iter(|| {
            let first = db
                .scan_stream(None, None)
                .expect("scan_stream")
                .next()
                .expect("a first row")
                .expect("scan row");
            black_box(first.0.len())
        })
    });
    group.bench_function("materialize", |b| {
        b.iter(|| {
            let rows = db.scan(None, None).expect("scan");
            black_box(rows.first().expect("a first row").0.len())
        })
    });
    group.finish();

    // The shape most callers have: read a page and stop.
    let mut group = c.benchmark_group("scan_stream/first_page_32MiB");
    group.throughput(Throughput::Elements(PAGE as u64));
    group.bench_function("stream", |b| {
        b.iter(|| {
            let taken: usize = db
                .scan_stream(None, None)
                .expect("scan_stream")
                .take(PAGE)
                .map(|entry| {
                    let (key, value) = entry.expect("scan row");
                    key.len() + value.len()
                })
                .sum();
            black_box(taken)
        })
    });
    group.bench_function("materialize", |b| {
        b.iter(|| {
            let rows = db.scan(None, None).expect("scan");
            let taken: usize = rows
                .iter()
                .take(PAGE)
                .map(|(key, value)| key.len() + value.len())
                .sum();
            black_box(taken)
        })
    });
    group.finish();

    if is_test_run() {
        return;
    }

    // Peak live bytes, measured outside criterion so each side is one clean
    // region rather than an average over iterations.
    let (streamed, stream_peak) = peak_live_bytes(|| stream_all(&db));
    let (materialized, materialize_peak) = peak_live_bytes(|| materialize_all(&db));
    assert_eq!(streamed, N_KEYS, "streaming scan missed keys");
    assert_eq!(materialized, N_KEYS, "materializing scan missed keys");

    let (paged, page_peak) = peak_live_bytes(|| {
        db.scan_stream(None, None)
            .expect("scan_stream")
            .take(PAGE)
            .count() as u64
    });
    assert_eq!(paged, PAGE as u64, "early stop returned the wrong count");

    let ratio = if stream_peak == 0 {
        0.0
    } else {
        materialize_peak as f64 / stream_peak as f64
    };
    common::write_family(
        "scan_stream",
        &format!(
            "{{\"keys\":{N_KEYS},\"value_bytes\":{VALUE_LEN},\
             \"scanned_bytes\":{},\"page\":{PAGE},\
             \"stream_peak_bytes\":{stream_peak},\
             \"materialize_peak_bytes\":{materialize_peak},\
             \"page_peak_bytes\":{page_peak},\
             \"peak_ratio\":{}}}",
            scanned_bytes(),
            num(ratio),
        ),
    );
}

criterion_group!(benches, scan_stream);
criterion_main!(benches);
