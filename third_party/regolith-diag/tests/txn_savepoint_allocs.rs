//! A savepoint is a mark on the write buffer, not a copy of it.
//!
//! Setting one is O(1) and rolling back costs the writes it discards, so a
//! transaction that holds a large buffer and sets many savepoints pays for the
//! savepoints and not for the buffer, once per savepoint. Measured with a
//! counting global allocator rather than a timer: a copy of the buffer shows
//! up as bytes, exactly and independent of machine load.

// Native-only. wasm-pack builds every test target for wasm32, and this uses
// the filesystem.
#![cfg(not(target_arch = "wasm32"))]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use regolith::{OptimisticTransactionDb, Options, TxnOptions};
use tempfile::TempDir;

// Per thread, so only the measuring thread's allocations count: the database
// opens background threads whose allocations must not leak into a figure.
// Const-initialised and without a destructor, so touching them allocates
// nothing.
thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
    static BYTES: Cell<usize> = const { Cell::new(0) };
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count(bytes: usize) {
    if ARMED.get() {
        BYTES.set(BYTES.get() + bytes);
        COUNT.set(COUNT.get() + 1);
    }
}

// SAFETY: every method forwards to `System`, which is a correct allocator, and
// only adds relaxed counter updates around it. No pointer or layout is
// altered, so the `GlobalAlloc` contract is exactly `System`'s.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size.saturating_sub(layout.size()));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const BUFFERED: usize = 2_000;
const SAVEPOINTS: usize = 10_000;

/// Run `f` and return the bytes and the allocations it made.
fn measure(f: impl FnOnce()) -> (usize, usize) {
    BYTES.set(0);
    COUNT.set(0);
    ARMED.set(true);
    f();
    ARMED.set(false);
    (BYTES.get(), COUNT.get())
}

#[test]
fn ten_thousand_savepoints_over_a_large_buffer_copy_nothing() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let mut txn = db.begin(&TxnOptions::new());
    let value = vec![0xA5u8; 256];
    for i in 0..BUFFERED {
        txn.put(format!("key/{i:06}").as_bytes(), &value).unwrap();
    }

    let (bytes, allocations) = measure(|| {
        for _ in 0..SAVEPOINTS {
            txn.set_savepoint();
        }
    });
    // One machine word per savepoint, grown by doubling: far below even one
    // copy of the buffer, which a snapshot per savepoint would repeat 10,000
    // times.
    let word = std::mem::size_of::<usize>();
    assert!(
        bytes <= 4 * SAVEPOINTS * word,
        "{SAVEPOINTS} savepoints allocated {bytes} bytes in {allocations} allocations"
    );
    assert!(allocations <= 64, "{allocations} allocations");

    let (bytes, _) = measure(|| {
        for _ in 0..SAVEPOINTS {
            txn.rollback_to_savepoint().unwrap();
        }
    });
    assert_eq!(bytes, 0, "rolling back allocated");
    assert_eq!(txn.get(b"key/000000").unwrap().as_deref(), Some(&value[..]));
}

#[test]
fn a_set_write_rollback_cycle_costs_the_write_and_not_the_buffer() {
    let dir = TempDir::new().unwrap();
    let db = OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap();
    let mut txn = db.begin(&TxnOptions::new());
    let value = vec![0xA5u8; 256];
    for i in 0..BUFFERED {
        txn.put(format!("key/{i:06}").as_bytes(), &value).unwrap();
    }
    let (bytes, _) = measure(|| {
        for i in 0..1_000u32 {
            txn.set_savepoint();
            txn.put(&i.to_be_bytes(), b"scratch").unwrap();
            txn.rollback_to_savepoint().unwrap();
        }
    });
    // A copy of the buffer per cycle would be megabytes per iteration.
    assert!(bytes < 1_000 * 512, "1,000 cycles allocated {bytes} bytes");
}
