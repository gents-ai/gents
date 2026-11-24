//! A scan must not read the range a caller skipped.
//!
//! `Db::scan` materializes the whole range up front, which is bounded by
//! the data rather than by the caller: on an embedded budget a scan of a
//! large collection is simply not affordable. `Db::scan_stream` holds one
//! entry, so stopping early costs only what was read.
//!
//! One test per binary, deliberately. The counters are process-global and
//! Rust runs a binary's tests on parallel threads, so a second test in
//! this file would land its allocations inside the measurement window and
//! quietly wreck the number.

// Native-only. wasm-pack builds every test target for wasm32, and this
// uses the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regolith::{Db, Options};
use tempfile::TempDir;

static ARMED: AtomicBool = AtomicBool::new(false);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static COUNT: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System`, which is a correct
// allocator, and only adds relaxed counter updates around it. No pointer
// or layout is altered, so the `GlobalAlloc` contract is exactly
// `System`'s.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc(layout);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                BYTES.fetch_add(layout.size(), Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            p
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let p = System.realloc(ptr, layout, new_size);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            p
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc_zeroed(layout);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                BYTES.fetch_add(layout.size(), Ordering::Relaxed);
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            p
        }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const ENTRIES: usize = 5_000;
const VALUE_LEN: usize = 1024;

/// Taking a handful of entries must cost a handful of entries, not the
/// range. Measured against `Db::scan` over the same range, which reads
/// all of it.
#[test]
fn scan_stream_does_not_read_the_range_it_skips() {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), Options::default()).expect("open");
    let value = vec![b'x'; VALUE_LEN];
    for i in 0..ENTRIES {
        db.put(format!("key/{i:06}").as_bytes(), &value)
            .expect("put");
    }

    BYTES.store(0, Ordering::Relaxed);
    COUNT.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    let taken = db
        .scan_stream(None, None)
        .expect("scan_stream")
        .take(5)
        .try_fold(0usize, |taken, entry| entry.map(|_| taken + 1))
        .expect("scan rows");
    ARMED.store(false, Ordering::Relaxed);
    let streamed = BYTES.load(Ordering::Relaxed);

    BYTES.store(0, Ordering::Relaxed);
    COUNT.store(0, Ordering::Relaxed);
    ARMED.store(true, Ordering::Relaxed);
    let all = db.scan(None, None).expect("scan").len();
    ARMED.store(false, Ordering::Relaxed);
    let materialized = BYTES.load(Ordering::Relaxed);

    assert_eq!(taken, 5);
    assert_eq!(all, ENTRIES);
    eprintln!(
        "take(5) streamed {streamed} bytes; scan materialized {materialized} bytes \
         over {ENTRIES} x {VALUE_LEN}B"
    );
    assert!(
        streamed * 10 < materialized,
        "take(5) allocated {streamed} bytes against {materialized} for the whole scan, \
         so scan_stream is reading the range it was asked to skip"
    );
}
