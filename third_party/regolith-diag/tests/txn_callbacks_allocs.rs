//! What transaction callbacks cost in allocations.
//!
//! A transaction that registered nothing, under a database with no hooks, must
//! allocate nothing for them; one that registered up to four of a kind, each
//! capturing a few words, allocates nothing for them either (an `on_abort`
//! callback needs the claim `close` shares), and only the fifth of a kind
//! spills to the heap.
//! Measured with a counting global allocator rather than a timer, so the
//! numbers are exact and independent of machine load.

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
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    if ARMED.get() {
        COUNT.set(COUNT.get() + 1);
    }
}

// SAFETY: every method forwards to `System`, which is a correct allocator, and
// only adds relaxed counter updates around it. No pointer or layout is
// altered, so the `GlobalAlloc` contract is exactly `System`'s.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Run `f` and return how many allocations it made.
fn allocations(f: impl FnOnce()) -> usize {
    COUNT.set(0);
    ARMED.set(true);
    f();
    ARMED.set(false);
    COUNT.get()
}

fn open() -> (OptimisticTransactionDb, TempDir) {
    let dir = TempDir::new().unwrap();
    (
        OptimisticTransactionDb::open(dir.path(), Options::default()).unwrap(),
        dir,
    )
}

#[test]
fn prepare_allocates_nothing_without_callbacks() {
    let (db, _dir) = open();
    let mut txn = db.begin(&TxnOptions::new());
    assert_eq!(allocations(|| txn.prepare().unwrap()), 0);
}

/// Plan 3.16 item 8: callbacks live in a small inline vector that allocates
/// only from the fifth callback onward. Up to four of each kind, each
/// capturing a few words, allocate nothing; the fifth of a kind spills.
#[test]
fn the_first_four_callbacks_of_a_kind_allocate_nothing() {
    let (db, _dir) = open();

    let mut txn = db.begin(&TxnOptions::new());
    let four = allocations(|| {
        for _ in 0..4 {
            txn.on_commit(|_| {});
        }
    });
    let fifth = allocations(|| txn.on_commit(|_| {}));
    assert_eq!(four, 0, "four on_commit callbacks live in the transaction");
    assert!(fifth >= 1, "the fifth spills to the heap");
    // Dropping a transaction that registered only `on_commit` callbacks runs
    // none of them and needs no claim.
    drop(txn);

    let mut txn = db.begin(&TxnOptions::new());
    let before = allocations(|| {
        for _ in 0..4 {
            txn.before_commit(|_| Ok(()));
        }
    });
    assert_eq!(before, 0, "four before_commit callbacks, no allocation");
    assert_eq!(
        allocations(|| txn.prepare().unwrap()),
        0,
        "running them allocates nothing"
    );
}

/// A closure that captures more than the few words a callback keeps in
/// place is boxed on its own: one allocation, and the ones beside it still
/// allocate nothing.
#[test]
fn a_callback_that_captures_much_is_boxed_on_its_own() {
    let (db, _dir) = open();
    let mut txn = db.begin(&TxnOptions::new());
    let big = [7u64; 16];
    let boxed = allocations(|| {
        txn.on_commit(move |_| {
            let _held = &big;
        })
    });
    let small = allocations(|| txn.on_commit(|_| {}));
    assert_eq!((boxed, small), (1, 0));
}

/// An `on_abort` callback is shared with `close`, which may run it from
/// another thread, so it lives in the transaction's claim: the first
/// registration makes the claim, and the next three live in it and
/// allocate nothing. The fifth spills.
#[test]
fn the_first_on_abort_makes_the_claim_and_the_next_three_allocate_nothing() {
    let (db, _dir) = open();
    let mut txn = db.begin(&TxnOptions::new());
    let first = allocations(|| txn.on_abort(|_| {}));
    let next_three = allocations(|| {
        for _ in 0..3 {
            txn.on_abort(|_| {});
        }
    });
    let fifth = allocations(|| txn.on_abort(|_| {}));
    assert!(first >= 1, "the claim close finds the transaction by");
    assert_eq!(next_three, 0, "the first four live in the claim");
    assert!(fifth >= 1, "the fifth spills to the heap");
}
