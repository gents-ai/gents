//! A transactional scan below `Serializable` holds no memory per key it
//! yielded.
//!
//! One test per binary: see `scan_stream_allocs.rs` for why.

#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use regolith::{IsolationLevel, OptimisticTransactionDb, Options, TxnOptions};
use tempfile::TempDir;

static ARMED: AtomicBool = AtomicBool::new(false);
static LIVE: AtomicIsize = AtomicIsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System` and only adds relaxed counter
// updates around it.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc(layout);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            }
            p
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            if ARMED.load(Ordering::Relaxed) {
                LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            }
            System.dealloc(ptr, layout)
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let p = System.realloc(ptr, layout, new_size);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                LIVE.fetch_add(
                    new_size as isize - layout.size() as isize,
                    Ordering::Relaxed,
                );
            }
            p
        }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc_zeroed(layout);
            if !p.is_null() && ARMED.load(Ordering::Relaxed) {
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            }
            p
        }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const ENTRIES: usize = 20_000;
const BUDGET: isize = 64 * 1024;

#[test]
fn a_scan_below_serializable_holds_no_memory_per_key() {
    let dir = TempDir::new().expect("tempdir");
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).expect("open");
    for i in 0..ENTRIES {
        db.db()
            .put(format!("key/{i:06}").as_bytes(), &[b'v'; 16])
            .expect("put");
    }
    for level in [
        IsolationLevel::ReadCommitted,
        IsolationLevel::SnapshotIsolation,
        IsolationLevel::RepeatableRead,
    ] {
        let txn = db.begin(&TxnOptions::new().isolation(level));
        LIVE.store(0, Ordering::Relaxed);
        ARMED.store(true, Ordering::Relaxed);
        let yielded = txn.scan_stream(None, None).count();
        ARMED.store(false, Ordering::Relaxed);
        let retained = LIVE.load(Ordering::Relaxed);
        assert_eq!(yielded, ENTRIES);
        eprintln!("{level:?}: a full scan of {ENTRIES} keys left {retained} bytes held");
        assert!(
            retained < BUDGET,
            "{level:?}: a full scan of {ENTRIES} keys left {retained} bytes held by \
             the transaction, so it is keeping memory per key"
        );
        txn.commit().expect("read-only commit");
    }
}
