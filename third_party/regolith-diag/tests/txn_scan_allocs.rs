//! A scan inside a transaction must stream the database side too.
//!
//! The transaction's own writes are bounded by what it wrote, so sorting
//! those up front is fine. The committed side is not bounded by anything
//! the transaction controls, so it has to stay a cursor.
//!
//! One test per binary: see `scan_stream_allocs.rs` for why.

// Native-only. wasm-pack builds every test target for wasm32, and this
// uses the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use regolith::{IsolationLevel, OptimisticTransactionDb, Options, TxnOptions};
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

#[test]
fn a_transaction_scan_stops_early_without_reading_the_range() {
    let dir = TempDir::new().expect("tempdir");
    let db = Arc::new(OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open"));
    let value = vec![b'x'; VALUE_LEN];
    for i in 0..ENTRIES {
        db.db()
            .put(format!("key/{i:06}").as_bytes(), &value)
            .expect("put");
    }
    for level in [
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
        IsolationLevel::Serializable,
    ] {
        let txn = db.begin(&TxnOptions::new().isolation(level));

        BYTES.store(0, Ordering::Relaxed);
        COUNT.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        let taken = txn.scan_stream(None, None).take(5).count();
        ARMED.store(false, Ordering::Relaxed);
        let streamed = BYTES.load(Ordering::Relaxed);

        assert_eq!(taken, 5);
        let payload = ENTRIES * VALUE_LEN;
        eprintln!(
            "{level:?}: txn take(5) streamed {streamed} bytes in {} allocations over {ENTRIES} x {VALUE_LEN}B",
            COUNT.load(Ordering::Relaxed)
        );
        assert!(
            streamed * 10 < payload,
            "{level:?}: a transaction scan that stopped after 5 entries allocated {streamed} bytes \
             against a {payload} byte range, so it is reading the range instead of \
             streaming it"
        );
    }
}
