//! Allocation budgets for regolith's hot paths, enforced as a gate.
//!
//! Run with `cargo bench --bench allocs`. The process exits non-zero as
//! soon as any path allocates more per operation than its budget, so
//! this binary sits in CI next to the test suite.
//!
//! Counting is gated two ways at once. A process-wide atomic switches
//! the allocator hook on only for the measured region, so opening a
//! database, filling it and tearing it down are never attributed to a
//! path. The counters themselves are thread-local, so background
//! compaction, which runs on its own thread, is excluded even when it
//! overlaps the measured region. A memtable flush is not background
//! work: `rotate_if_full` runs on the group leader's own thread, so a
//! flush inside a measured region is counted. The gated `put` rows use
//! the default 64 MiB write buffer and never rotate; `rotation_disclosure`
//! measures a configuration that does.
//!
//! Every path is measured twice, because subtracting a harness cost is
//! only as honest as the control that estimates it. The `sub` run builds
//! its keys with `format!` inside the measured loop, matching the shape
//! of the recorded `d1ec2e7` baseline, and a control loop that builds
//! the same keys and does nothing else is subtracted from it. The `pre`
//! run builds the keys before the region opens, so it needs no
//! subtraction and no control can flatter it. The gate uses whichever of
//! the two is larger.

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use regolith::{Db, DurabilityMode, OptimisticTransactionDb, Options, TxnOptions, WriteBatch};
use tempfile::TempDir;

/// Process-wide counting gate. Setup and teardown run with this off.
static COUNTING: AtomicBool = AtomicBool::new(false);

thread_local! {
    /// Const-initialized and `Drop`-free, so the allocator hook can
    /// touch it without lazy initialization allocating re-entrantly.
    static ALLOCS: Cell<u64> = const { Cell::new(0) };
    static BYTES: Cell<u64> = const { Cell::new(0) };
}

fn record(bytes: u64) {
    let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
    let _ = BYTES.try_with(|c| c.set(c.get() + bytes));
}

struct Counting;

// SAFETY: every method forwards to `System` unchanged and returns its
// pointer verbatim; the counters are thread-local `Cell<u64>` updates
// that cannot allocate or unwind.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            record(layout.size() as u64);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            record(layout.size() as u64);
        }
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            record(new_size as u64);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations and bytes attributed to one measured region.
#[derive(Clone, Copy, Default)]
struct Counts {
    allocs: u64,
    bytes: u64,
}

impl Counts {
    fn minus(self, other: Self) -> Self {
        Self {
            allocs: self.allocs.saturating_sub(other.allocs),
            bytes: self.bytes.saturating_sub(other.bytes),
        }
    }

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
    COUNTING.store(true, Ordering::SeqCst);
    let out = f();
    COUNTING.store(false, Ordering::SeqCst);
    let counts = Counts {
        allocs: ALLOCS.with(|c| c.get()),
        bytes: BYTES.with(|c| c.get()),
    };
    (out, counts)
}

const OPS: usize = 20_000;
const VALUE_LEN: usize = 256;
const BATCH: usize = 64;

fn key_at(i: usize) -> String {
    format!("key{:08}", i)
}

fn missing_key_at(i: usize) -> String {
    format!("nope{:08}", i)
}

/// Cost of building `ops` keys with `key` and nothing else, so the
/// harness's own allocation can be subtracted from a measured path.
fn key_control(ops: usize, key: fn(usize) -> String) -> Counts {
    measure(|| {
        for i in 0..ops {
            black_box(key(i));
        }
    })
    .1
}

/// Emit the measured budgets as a metric family, so this bench feeds the
/// run document the perf site is built from like every other one.
///
/// Without this the family is missing from the run: the harness writes it
/// only when a bench asks, and `allocs` was the one that never did, so its
/// CI job printed a perfect table and then failed with "No files were
/// found with the provided path".
fn emit_family(rows: &[Row]) {
    let paths: Vec<String> = rows
        .iter()
        .map(|r| {
            format!(
                "{{\"path\":\"{}\",\"budget\":{},\"achieved\":{},\"bytes_per_op\":{},\
                 \"met\":{}}}",
                r.path,
                fmt_f64(r.budget),
                fmt_f64(r.achieved()),
                fmt_f64(r.direct().1),
                r.met(),
            )
        })
        .collect();
    common::write_family(
        "allocs",
        &format!(
            "{{\"ops\":{},\"paths\":[{}]}}",
            rows.first().map_or(0, |r| r.ops),
            paths.join(",")
        ),
    );
}

/// JSON has no NaN or infinity, so a value that is not finite is emitted
/// as null rather than as a token no parser accepts.
fn fmt_f64(v: f64) -> String {
    if v.is_finite() {
        format!("{v:.4}")
    } else {
        "null".to_string()
    }
}

/// One measured path: what it cost, measured two independent ways, and
/// what it is allowed to cost.
struct Row {
    path: &'static str,
    baseline: Option<f64>,
    budget: f64,
    ops: u64,
    gross: Counts,
    harness: Counts,
    prebuilt: Counts,
}

impl Row {
    /// Keys built inside the region, the control subtracted off.
    fn subtracted(&self) -> (f64, f64) {
        self.gross.minus(self.harness).per_op(self.ops)
    }

    /// Keys built before the region, nothing subtracted.
    fn direct(&self) -> (f64, f64) {
        self.prebuilt.per_op(self.ops)
    }

    /// The worse of the two, so neither an over-generous control nor a
    /// lucky prebuilt run can move the gate in our favour.
    fn achieved(&self) -> f64 {
        self.subtracted().0.max(self.direct().0)
    }

    fn met(&self) -> bool {
        self.achieved() <= self.budget
    }
}

/// A database plus the temp directory keeping it alive.
struct Fixture {
    db: Db,
    _dir: TempDir,
}

fn options() -> Options {
    Options::default()
        // Comfortably larger than the data set, so a warm read never
        // re-reads an evicted block.
        .block_cache_size(256 * 1024 * 1024)
}

fn open() -> Fixture {
    let dir = TempDir::new().expect("tempdir");
    let db = Db::open(dir.path(), options()).expect("open");
    Fixture { db, _dir: dir }
}

/// `OPS` keys with `VALUE_LEN`-byte values. With `to_sstables` the
/// memtable is flushed and compacted first, so reads land on an
/// SSTable rather than the skip list.
fn filled(to_sstables: bool) -> Fixture {
    let f = open();
    let value = vec![b'v'; VALUE_LEN];
    for i in 0..OPS {
        f.db.put(key_at(i).as_bytes(), &value).expect("put");
    }
    if to_sstables {
        f.db.compact_range(None, None).wait().expect("compact");
    }
    // Without this the two residency variants could silently measure the
    // same path: one reads an SSTable only if the memtable is actually
    // empty, and the other reads the memtable only if no SSTable exists.
    let sst =
        f.db.get_int_property("regolith.total-sst-files-size")
            .unwrap_or_default();
    let mem =
        f.db.get_int_property("regolith.cur-size-all-mem-tables")
            .unwrap_or_default();
    if to_sstables {
        assert!(sst > 0 && mem == 0, "not SSTable-resident: {sst} / {mem}");
    } else {
        assert!(sst == 0 && mem > 0, "not memtable-resident: {sst} / {mem}");
    }
    f
}

/// Measure one read path against an already-populated database, both
/// with keys built inside the region and with keys built before it.
fn read_row(
    path: &'static str,
    baseline: Option<f64>,
    budget: f64,
    f: &Fixture,
    key: fn(usize) -> String,
    body: impl Fn(&Db, &[u8]),
) -> Row {
    // Warm whatever the path touches on first use, so neither measured
    // region pays a cost a steady-state read would not.
    for i in 0..OPS {
        body(&f.db, key(i).as_bytes());
    }

    let (_, gross) = measure(|| {
        for i in 0..OPS {
            body(&f.db, key(i).as_bytes());
        }
    });
    let harness = key_control(OPS, key);

    let keys: Vec<String> = (0..OPS).map(key).collect();
    let (_, prebuilt) = measure(|| {
        for k in &keys {
            body(&f.db, k.as_bytes());
        }
    });

    Row {
        path,
        baseline,
        budget,
        ops: OPS as u64,
        gross,
        harness,
        prebuilt,
    }
}

fn get_rows() -> Vec<Row> {
    let sst = filled(true);
    let mem = filled(false);
    let hit = |db: &Db, k: &[u8]| {
        assert_eq!(db.get(k).expect("get").expect("present").len(), VALUE_LEN);
    };
    let hit_slice = |db: &Db, k: &[u8]| {
        assert_eq!(
            db.get_slice(k).expect("get_slice").expect("present").len(),
            VALUE_LEN
        );
    };
    vec![
        read_row(
            "get, SSTable, cache warm",
            Some(21.52),
            3.0,
            &sst,
            key_at,
            hit,
        ),
        read_row("get, memtable resident", Some(6.00), 3.0, &mem, key_at, hit),
        read_row(
            "get, miss",
            Some(5.00),
            2.0,
            &sst,
            missing_key_at,
            |db, k| {
                assert!(db.get(k).expect("get").is_none());
            },
        ),
        read_row(
            "get_slice, SSTable, cache warm",
            None,
            1.0,
            &sst,
            key_at,
            hit_slice,
        ),
        read_row(
            "get_slice, memtable resident",
            None,
            1.0,
            &mem,
            key_at,
            hit_slice,
        ),
    ]
}

fn put_single() -> Row {
    let value = vec![b'v'; VALUE_LEN];

    let a = open();
    let (_, gross) = measure(|| {
        for i in 0..OPS {
            a.db.put(key_at(i).as_bytes(), &value).expect("put");
        }
    });

    let b = open();
    let keys: Vec<String> = (0..OPS).map(key_at).collect();
    let (_, prebuilt) = measure(|| {
        for k in &keys {
            b.db.put(k.as_bytes(), &value).expect("put");
        }
    });

    Row {
        path: "put, single",
        baseline: Some(9.00),
        budget: 3.0,
        ops: OPS as u64,
        gross,
        harness: key_control(OPS, key_at),
        prebuilt,
    }
}

fn put_write_batch() -> Row {
    let value = vec![b'v'; VALUE_LEN];
    let batches = OPS.div_ceil(BATCH);
    let ops = (batches * BATCH) as u64;

    let a = open();
    let (_, gross) = measure(|| {
        for b in 0..batches {
            let mut batch = WriteBatch::new();
            for j in 0..BATCH {
                batch.put(key_at(b * BATCH + j).as_bytes(), &value);
            }
            a.db.write(batch).expect("write");
        }
    });

    let c = open();
    let keys: Vec<String> = (0..ops as usize).map(key_at).collect();
    let (_, prebuilt) = measure(|| {
        for b in 0..batches {
            let mut batch = WriteBatch::new();
            for j in 0..BATCH {
                batch.put(keys[b * BATCH + j].as_bytes(), &value);
            }
            c.db.write(batch).expect("write");
        }
    });

    Row {
        path: "put, WriteBatch of 64",
        baseline: Some(8.09),
        budget: 3.0,
        ops,
        gross,
        // The batch loop builds exactly one key per op, same as the
        // single-put loop.
        harness: key_control(ops as usize, key_at),
        prebuilt,
    }
}

/// Walk the whole database front to back, touching every key and value,
/// and return the number of steps. Shared by the gated row and the
/// cold-cache disclosure so the two cannot measure different loops.
fn drain_iter(db: &Db) -> u64 {
    let mut it = db.iter();
    it.seek_to_first();
    let mut steps = 0u64;
    while it.valid() {
        assert!(!it.key().expect("key").is_empty());
        assert_eq!(it.value().expect("value").len(), VALUE_LEN);
        steps += 1;
        it.next();
    }
    steps
}

fn iterator_step(path: &'static str, to_sstables: bool) -> Row {
    let f = filled(to_sstables);
    // The gated row is the warm-cache figure, so the first walk, which
    // faults every block in from disk, runs outside the region.
    assert_eq!(drain_iter(&f.db), OPS as u64);

    let (steps, gross) = measure(|| drain_iter(&f.db));
    assert_eq!(steps, OPS as u64);

    Row {
        path,
        baseline: Some(2.16),
        budget: 1.0,
        ops: steps,
        gross,
        // The loop constructs no keys, so there is nothing to subtract
        // and a prebuilt variant would measure the same region twice.
        harness: Counts::default(),
        prebuilt: gross,
    }
}

/// Split the write path at the public API boundary: what `WriteBatch`
/// allocates on the caller's thread before the engine sees anything, and
/// what the engine adds on top. A `put` miss belongs to whichever half
/// is over, and this names it without needing symbols.
fn write_path_breakdown() {
    let value = vec![b'v'; VALUE_LEN];
    let batches = OPS.div_ceil(BATCH);
    let ops = (batches * BATCH) as u64;
    let keys: Vec<String> = (0..ops as usize).map(key_at).collect();

    let (_, build_only) = measure(|| {
        for b in 0..batches {
            let mut batch = WriteBatch::new();
            for j in 0..BATCH {
                batch.put(keys[b * BATCH + j].as_bytes(), &value);
            }
            black_box(batch);
        }
    });

    let f = open();
    let prebuilt: Vec<WriteBatch> = (0..batches)
        .map(|b| {
            let mut batch = WriteBatch::new();
            for j in 0..BATCH {
                batch.put(keys[b * BATCH + j].as_bytes(), &value);
            }
            batch
        })
        .collect();
    let (_, apply_only) = measure(|| {
        for batch in prebuilt {
            f.db.write(batch).expect("write");
        }
    });

    println!("\nWrite-path breakdown (prebuilt keys, nothing subtracted)");
    println!(
        "{:<44} {:>10} {:>9} {:>11}",
        "region", "raw allocs", "per op", "bytes/op"
    );
    for (name, counts) in [
        ("WriteBatch::put, built and dropped", build_only),
        ("Db::write of a prebuilt batch", apply_only),
    ] {
        let (allocs, bytes) = counts.per_op(ops);
        println!(
            "{name:<44} {:>10} {allocs:>9.4} {bytes:>11.1}",
            counts.allocs
        );
    }
    let per_batch = build_only.allocs as f64 / batches as f64;
    let copies = 2.0 * BATCH as f64;
    println!("  {per_batch:.3} allocs per batch of {BATCH}: {copies:.0} key/value copies plus");
    println!(
        "  {:.3} for the batch's own Vec growth.",
        per_batch - copies
    );
    println!("  The two copies per op are src/lib.rs:2201 prefix_key(..) and");
    println!("  src/lib.rs:2202 value.to_vec() in WriteBatch::put, mirrored at");
    println!("  src/lib.rs:603-604 in Db::put_opt. Both copy on the caller's thread");
    println!("  before the engine is entered.");
}

/// What the warm iterator figure leaves out.
///
/// The gated rows walk the database once before the measured region, so
/// every data block is already in the cache. The first walk after a
/// compaction is not free, and this prints it rather than letting the
/// warm number stand for both.
fn iterator_disclosure() {
    println!("\nDisclosure: iterator on a cold block cache versus a warm one");
    for (name, to_sstables) in [("SSTable", true), ("memtable", false)] {
        let f = filled(to_sstables);
        let (cold_steps, cold) = measure(|| drain_iter(&f.db));
        let (warm_steps, warm) = measure(|| drain_iter(&f.db));
        let cache =
            f.db.get_int_property("regolith.block-cache-usage")
                .unwrap_or_default();
        println!(
            "  {name}: cold {} allocs over {cold_steps} steps ({:.5}/step), \
             warm {} allocs ({:.5}/step), block cache {cache} B",
            cold.allocs,
            cold.allocs as f64 / cold_steps as f64,
            warm.allocs,
            warm.allocs as f64 / warm_steps as f64
        );
    }
}

/// What the gated `put` figure leaves out.
///
/// The budgeted run uses the default 64 MiB write buffer, so 20000
/// 256-byte values never fill a memtable and no rotation happens inside
/// the measured region. Rotation calls `flush_frozen_memtable` on the
/// writer's own thread, so its cost is real and lands on the caller.
/// This run shrinks the write buffer until rotation is unavoidable and
/// prints the amortized figure, so the gated number is never mistaken
/// for the whole cost of a write.
fn rotation_disclosure() {
    let dir = TempDir::new().expect("tempdir");
    let opts = Options::default()
        .write_buffer_size(1024 * 1024)
        .block_cache_size(64 * 1024 * 1024);
    let db = Db::open(dir.path(), opts).expect("open");
    let value = vec![b'v'; VALUE_LEN];
    let keys: Vec<String> = (0..OPS).map(key_at).collect();

    let (_, counts) = measure(|| {
        for k in &keys {
            db.put(k.as_bytes(), &value).expect("put");
        }
    });
    let (allocs, bytes) = counts.per_op(OPS as u64);
    let sst_bytes = db
        .get_int_property("regolith.total-sst-files-size")
        .unwrap_or_default();
    assert!(
        sst_bytes > 0,
        "no rotation happened, the disclosure is void"
    );
    println!("\nDisclosure: put with memtable rotation on the writer's thread");
    println!("  1 MiB write buffer, prebuilt keys, {sst_bytes} SSTable bytes written:");
    println!("  {allocs:.3} allocs/op, {bytes:.1} B/op");
    println!("  The gated `put` rows use the default 64 MiB buffer and rotate zero");
    println!("  times, so they exclude this cost.");
}

/// The commit paths the gated rows do not read.
///
/// The gated `put` rows commit at the default durability, where no commit
/// waits for a sync. These are the same single write with the sync, and a
/// transaction of one key committed both ways: what the commit pipeline
/// costs a caller beyond the copies of the key and the value.
fn commit_disclosure() {
    let value = vec![b'v'; VALUE_LEN];
    let keys: Vec<String> = (0..OPS).map(key_at).collect();
    println!("\nDisclosure: commit paths outside the gated rows (prebuilt keys)");
    println!("{:<44} {:>9} {:>11}", "path", "allocs/op", "bytes/op");
    for (name, durability) in [
        ("put, single, Eventual", DurabilityMode::Eventual),
        ("put, single, Immediate", DurabilityMode::Immediate),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let db = Db::open(dir.path(), options().durability(durability)).expect("open");
        // The first write pays for what a thread keeps between commits.
        db.put(b"warm", &value).expect("put");
        let (_, counts) = measure(|| {
            for k in &keys {
                db.put(k.as_bytes(), &value).expect("put");
            }
        });
        let (allocs, bytes) = counts.per_op(OPS as u64);
        println!("{name:<44} {allocs:>9.4} {bytes:>11.1}");
    }
    for (name, durability) in [
        ("transaction of one key, Eventual", DurabilityMode::Eventual),
        (
            "transaction of one key, Immediate",
            DurabilityMode::Immediate,
        ),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let db = OptimisticTransactionDb::open(dir.path(), options().durability(durability))
            .expect("open");
        let txn = TxnOptions::new();
        let warm = db.begin(&txn);
        warm.put(b"warm", &value).expect("put");
        warm.commit().expect("commit");
        let (_, counts) = measure(|| {
            for k in &keys {
                let tx = db.begin(&txn);
                tx.put(k.as_bytes(), &value).expect("put");
                tx.commit().expect("commit");
            }
        });
        let (allocs, bytes) = counts.per_op(OPS as u64);
        println!("{name:<44} {allocs:>9.4} {bytes:>11.1}");
    }
    for (name, durability) in [
        (
            "commit_nowait of one key, Eventual",
            DurabilityMode::Eventual,
        ),
        (
            "commit_nowait of one key, Immediate",
            DurabilityMode::Immediate,
        ),
    ] {
        let dir = TempDir::new().expect("tempdir");
        let db = OptimisticTransactionDb::open(dir.path(), options().durability(durability))
            .expect("open");
        let mut queue = db.db().io_queue();
        let txn = TxnOptions::new().io_queue(queue.id());
        let warm = db.begin(&txn);
        warm.put(b"warm", &value).expect("put");
        queue.block_on(warm.commit_nowait()).expect("commit");
        let (_, counts) = measure(|| {
            for k in &keys {
                let tx = db.begin(&txn);
                tx.put(k.as_bytes(), &value).expect("put");
                queue.block_on(tx.commit_nowait()).expect("commit");
            }
        });
        let (allocs, bytes) = counts.per_op(OPS as u64);
        println!("{name:<44} {allocs:>9.4} {bytes:>11.1}");
    }
}

fn main() -> ExitCode {
    let mut rows = get_rows();
    rows.push(put_single());
    rows.push(put_write_batch());
    rows.push(iterator_step("iterator step, SSTable", true));
    rows.push(iterator_step("iterator step, memtable", false));

    println!(
        "Allocation budgets: {OPS} ops, {VALUE_LEN}-byte values, batches of {BATCH}, release build"
    );
    println!(
        "{:<32} {:>9} {:>7} {:>8} {:>8} {:>9} {:>9} {:>5}",
        "path", "baseline", "budget", "sub", "pre", "achieved", "bytes/op", "met"
    );
    let mut failed = Vec::new();
    for row in &rows {
        let (sub, _) = row.subtracted();
        let (pre, pre_bytes) = row.direct();
        let baseline = match row.baseline {
            Some(b) => format!("{b:.2}"),
            None => "n/a".to_string(),
        };
        println!(
            "{:<32} {:>9} {:>7.2} {:>8.4} {:>8.4} {:>9.4} {:>9.1} {:>5}",
            row.path,
            baseline,
            row.budget,
            sub,
            pre,
            row.achieved(),
            pre_bytes,
            if row.met() { "yes" } else { "NO" }
        );
        if !row.met() {
            failed.push((row.path, row.budget, row.achieved()));
        }
    }
    println!();
    println!("`sub` builds each key with format! inside the measured region and");
    println!("subtracts a control loop that builds the same keys; `pre` builds them");
    println!("first and subtracts nothing. `achieved` is the larger of the two and is");
    println!("what the gate reads, so an over-generous control cannot buy a pass.");
    println!("Background compaction runs on its own thread and is not counted; a");
    println!("memtable flush runs on the writer's thread and is counted, and the gated");
    println!("`put` rows never trigger one. The baseline column is the recorded");
    println!("d1ec2e7 figure from the brief; it was not re-measured here, and it folded");
    println!("in one harness allocation per op where this harness measures two.");

    write_path_breakdown();
    iterator_disclosure();
    rotation_disclosure();
    commit_disclosure();

    emit_family(&rows);

    if failed.is_empty() {
        println!("\nAll {} allocation budgets met.", rows.len());
        return ExitCode::SUCCESS;
    }
    eprintln!("\nALLOCATION BUDGET EXCEEDED");
    for (path, budget, actual) in &failed {
        eprintln!("  {path}: budget <= {budget:.2} allocs/op, measured {actual:.2} allocs/op");
    }
    ExitCode::FAILURE
}
