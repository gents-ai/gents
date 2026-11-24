//! Allocation budgets for the read path, enforced as tests.
//!
//! A global allocator is process-wide, so these gates live in their own
//! test binary. Counters are thread-local: each case measures only the
//! allocations the measuring thread makes, which is exactly the per-op
//! cost a caller pays. Background compaction allocates on its own
//! thread and is deliberately not counted.
//!
//! Each case reports two numbers. `harness` keys are built with
//! `format!("key{:08}", i)` per operation, matching the shape of the
//! recorded baseline (so the harness's own 1 allocation per op is
//! included in both); `prebuilt` keys are hoisted out of the loop, so
//! that column is regolith's own cost with nothing added.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use regolith::{Db, Options};
use tempfile::TempDir;

thread_local! {
    /// Const-initialized and `Drop`-free, so the allocator hook can
    /// touch it without lazy initialization allocating re-entrantly.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: every method forwards to `System` unchanged; the counters are
// thread-local `Cell<u64>` updates that cannot allocate or unwind.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        let _ = BYTES.try_with(|c| c.set(c.get() + layout.size() as u64));
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        let _ = BYTES.try_with(|c| c.set(c.get() + new_size as u64));
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations and bytes attributed to one measured region.
#[derive(Clone, Copy)]
struct Counts {
    allocs: u64,
    bytes: u64,
}

impl Counts {
    fn per_op(self, ops: u64) -> (f64, f64) {
        (
            self.allocs as f64 / ops as f64,
            self.bytes as f64 / ops as f64,
        )
    }
}

fn measure<R>(f: impl FnOnce() -> R) -> (R, Counts) {
    ALLOCS.with(|c| c.set(0));
    BYTES.with(|c| c.set(0));
    let out = f();
    let counts = Counts {
        allocs: ALLOCS.with(|c| c.get()),
        bytes: BYTES.with(|c| c.get()),
    };
    (out, counts)
}

const OPS: usize = 20_000;
const VALUE_LEN: usize = 256;

fn key_at(i: usize) -> String {
    format!("key{:08}", i)
}

fn missing_key_at(i: usize) -> String {
    format!("nope{:08}", i)
}

/// A database holding `OPS` keys with 256-byte values, plus the temp
/// directory keeping it alive.
struct Fixture {
    db: Db,
    _dir: TempDir,
}

fn fill(flush_to_sstables: bool) -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let opts = Options::default()
        // Comfortably larger than the whole data set so the warm case
        // is genuinely warm and eviction never re-reads a block.
        .block_cache_size(256 * 1024 * 1024);
    let db = Db::open(dir.path(), opts).expect("open");
    let value = vec![b'v'; VALUE_LEN];
    for i in 0..OPS {
        db.put(key_at(i).as_bytes(), &value).expect("put");
    }
    if flush_to_sstables {
        db.compact_range(None, None).wait().expect("compact");
    }
    Fixture { db, _dir: dir }
}

/// Report a case and assert its budget. Printed with `--nocapture`.
fn report(case: &str, counts: Counts, ops: u64, budget: f64) {
    let (allocs, bytes) = counts.per_op(ops);
    println!("{case:<44} {allocs:>6.2} allocs/op {bytes:>8.1} B/op  (budget <= {budget})");
    assert!(
        allocs <= budget,
        "{case}: {allocs:.2} allocs/op exceeds the budget of {budget}"
    );
}

#[test]
fn get_from_warm_sstable_is_within_budget() {
    let f = fill(true);
    let keys: Vec<String> = (0..OPS).map(key_at).collect();
    // Warm every block into the cache before measuring.
    for k in &keys {
        assert!(f.db.get(k.as_bytes()).expect("get").is_some());
    }

    let (_, harness) = measure(|| {
        for i in 0..OPS {
            let got = f.db.get(key_at(i).as_bytes()).expect("get");
            assert!(got.is_some());
        }
    });
    report("get, warm SSTable (format! key)", harness, OPS as u64, 3.0);

    let (_, prebuilt) = measure(|| {
        for k in &keys {
            let got = f.db.get(k.as_bytes()).expect("get");
            assert!(got.is_some());
        }
    });
    report(
        "get, warm SSTable (prebuilt key)",
        prebuilt,
        OPS as u64,
        3.0,
    );
}

#[test]
fn get_slice_from_warm_sstable_is_within_budget() {
    let f = fill(true);
    let keys: Vec<String> = (0..OPS).map(key_at).collect();
    for k in &keys {
        assert!(f.db.get_slice(k.as_bytes()).expect("get_slice").is_some());
    }

    let (_, prebuilt) = measure(|| {
        for k in &keys {
            let got = f.db.get_slice(k.as_bytes()).expect("get_slice");
            assert_eq!(got.expect("present").len(), VALUE_LEN);
        }
    });
    report("get_slice, warm SSTable", prebuilt, OPS as u64, 1.0);
}

#[test]
fn get_from_memtable_is_within_budget() {
    let f = fill(false);
    let keys: Vec<String> = (0..OPS).map(key_at).collect();

    let (_, harness) = measure(|| {
        for i in 0..OPS {
            assert!(f.db.get(key_at(i).as_bytes()).expect("get").is_some());
        }
    });
    report("get, memtable (format! key)", harness, OPS as u64, 3.0);

    let (_, prebuilt) = measure(|| {
        for k in &keys {
            assert!(f.db.get(k.as_bytes()).expect("get").is_some());
        }
    });
    report("get, memtable (prebuilt key)", prebuilt, OPS as u64, 3.0);
}

#[test]
fn get_miss_is_within_budget() {
    let f = fill(true);
    let keys: Vec<String> = (0..OPS).map(missing_key_at).collect();
    for k in &keys {
        assert!(f.db.get(k.as_bytes()).expect("get").is_none());
    }

    let (_, harness) = measure(|| {
        for i in 0..OPS {
            assert!(
                f.db.get(missing_key_at(i).as_bytes())
                    .expect("get")
                    .is_none()
            );
        }
    });
    report("get, miss (format! key)", harness, OPS as u64, 2.0);

    let (_, prebuilt) = measure(|| {
        for k in &keys {
            assert!(f.db.get(k.as_bytes()).expect("get").is_none());
        }
    });
    report("get, miss (prebuilt key)", prebuilt, OPS as u64, 2.0);

    let (_, slice) = measure(|| {
        for k in &keys {
            assert!(f.db.get_slice(k.as_bytes()).expect("get_slice").is_none());
        }
    });
    report("get_slice, miss (prebuilt key)", slice, OPS as u64, 1.0);
}

#[test]
fn has_and_get_size_are_within_budget() {
    let f = fill(true);
    let keys: Vec<String> = (0..OPS).map(key_at).collect();
    let missing: Vec<String> = (0..OPS).map(missing_key_at).collect();
    for k in &keys {
        assert!(f.db.has(k.as_bytes()).expect("has"));
    }

    let (_, has_hit) = measure(|| {
        for k in &keys {
            assert!(f.db.has(k.as_bytes()).expect("has"));
        }
    });
    report("has, warm SSTable hit", has_hit, OPS as u64, 1.0);

    let (_, has_miss) = measure(|| {
        for k in &missing {
            assert!(!f.db.has(k.as_bytes()).expect("has"));
        }
    });
    report("has, miss", has_miss, OPS as u64, 1.0);

    let (_, size) = measure(|| {
        for k in &keys {
            assert_eq!(
                f.db.get_size(k.as_bytes()).expect("get_size"),
                Some(VALUE_LEN)
            );
        }
    });
    report("get_size, warm SSTable hit", size, OPS as u64, 1.0);
}

#[test]
fn iterator_step_is_within_budget() {
    let f = fill(true);

    // Warm the block cache with one full pass.
    let mut warm = f.db.iter();
    warm.seek_to_first();
    let mut seen = 0usize;
    while warm.valid() {
        seen += 1;
        warm.next();
    }
    drop(warm);
    assert_eq!(seen, OPS);

    let (steps, counts) = measure(|| {
        let mut it = f.db.iter();
        it.seek_to_first();
        let mut steps = 0u64;
        while it.valid() {
            // Touch both halves so the compiler cannot elide the read.
            assert!(!it.key().expect("key").is_empty());
            assert_eq!(it.value().expect("value").len(), VALUE_LEN);
            steps += 1;
            it.next();
        }
        steps
    });
    assert_eq!(steps, OPS as u64);
    report("iterator step, SSTable", counts, steps, 1.0);
}

#[test]
fn iterator_step_over_memtable_is_within_budget() {
    let f = fill(false);

    let mut warm = f.db.iter();
    warm.seek_to_first();
    while warm.valid() {
        warm.next();
    }
    drop(warm);

    let (steps, counts) = measure(|| {
        let mut it = f.db.iter();
        it.seek_to_first();
        let mut steps = 0u64;
        while it.valid() {
            assert!(!it.key().expect("key").is_empty());
            assert_eq!(it.value().expect("value").len(), VALUE_LEN);
            steps += 1;
            it.next();
        }
        steps
    });
    assert_eq!(steps, OPS as u64);
    report("iterator step, memtable", counts, steps, 1.0);
}
