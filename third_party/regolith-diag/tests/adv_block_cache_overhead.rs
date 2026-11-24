//! `ENTRY_OVERHEAD` is the whole reason the byte budget bounds memory
//! rather than payload bytes. This measures what a cached entry really
//! costs the allocator and checks the budget still covers it.
//!
//! Two fresh child processes run the identical workload over the
//! identical data directory, one with a real block cache and one with
//! `block_cache_size = 0`. The difference in live heap is what the
//! block cache costs; `regolith.block-cache-usage` is what it claims to
//! cost. If the claim is under the cost, the budget stops bounding
//! memory and the over-allocation defect is back in a new costume.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use std::sync::Arc;

use regolith::{Db, Options, Statistics, Ticker};

static LIVE: AtomicIsize = AtomicIsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System` unchanged and only adds
// relaxed counter updates, so the `GlobalAlloc` contract is exactly
// `System`'s.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe {
            let p = System.alloc(layout);
            if !p.is_null() {
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            }
            p
        }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe {
            LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            System.dealloc(ptr, layout)
        }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        unsafe {
            let p = System.realloc(ptr, layout, new_size);
            if !p.is_null() {
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
            if !p.is_null() {
                LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
            }
            p
        }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Live heap once kovan has freed every node this thread retired.
///
/// Reclamation is deferred, and how much of it has happened by the time a
/// measurement is taken depends on how long the thread has been scheduled,
/// so an undrained reading moves with machine load. A flush frees what an
/// earlier epoch retired, so flush until the figure stops falling.
fn drained_live() -> isize {
    let mut live = LIVE.load(Ordering::Relaxed);
    loop {
        kovan::flush();
        let now = LIVE.load(Ordering::Relaxed);
        if now >= live {
            return now;
        }
        live = now;
    }
}

const KEYS: u32 = 60_000;
const DIR_ENV: &str = "REGOLITH_ADV_OVERHEAD_DIR";
const CACHE_ENV: &str = "REGOLITH_ADV_OVERHEAD_CACHE";
const BS_ENV: &str = "REGOLITH_ADV_OVERHEAD_BS";
const ROUNDS_ENV: &str = "REGOLITH_ADV_OVERHEAD_ROUNDS";

fn key(i: u32) -> Vec<u8> {
    format!("k{i:07}").into_bytes()
}

/// Child: open the prepared DB with or without a cache, read every
/// key twice, and report live heap against the cache's own claim.
#[test]
fn adv_overhead_child() {
    let (Ok(dir), Ok(cache)) = (std::env::var(DIR_ENV), std::env::var(CACHE_ENV)) else {
        return;
    };
    let cache_bytes: usize = cache.parse().expect("cache size");
    let block_size: usize = std::env::var(BS_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1024);
    let stats = Arc::new(Statistics::new());
    let db = Db::open(
        &dir,
        Options::default()
            .block_cache_size(cache_bytes)
            .block_cache_num_shard_bits(0)
            .block_size(block_size)
            // A worker thread can hold a retired node back for as long as it
            // goes unscheduled, which no flush on this thread can reach, so
            // the measurement would move with machine load.
            .max_background_compactions(0)
            .statistics(Some(Arc::clone(&stats))),
    )
    .unwrap();

    let rounds: u32 = std::env::var(ROUNDS_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let heap_open = drained_live();
    for _ in 0..rounds {
        for i in 0..KEYS {
            assert!(db.get(&key(i)).unwrap().is_some());
        }
    }
    let heap_warm = drained_live();
    let usage = db.get_int_property("regolith.block-cache-usage").unwrap();
    let capacity = db
        .get_int_property("regolith.block-cache-capacity")
        .unwrap();
    let adds = stats.get_ticker(Ticker::BlockCacheAdd);
    let hits = stats.get_ticker(Ticker::BlockCacheHit);
    let misses = stats.get_ticker(Ticker::BlockCacheMiss);
    println!(
        "ADVRESULT cache={cache_bytes} heap={} usage={usage} capacity={capacity} \
adds={adds} hits={hits} misses={misses}",
        heap_warm - heap_open
    );
    drop(db);
}

#[test]
fn a_cached_entry_costs_no_more_heap_than_it_is_charged() {
    if std::env::var(DIR_ENV).is_ok() {
        return;
    }
    let prepare = |block_size: usize| -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let db = Db::open(
            dir.path(),
            Options::default()
                .block_cache_size(0)
                .block_size(block_size)
                .write_buffer_size(4 * 1024 * 1024),
        )
        .unwrap();
        for i in 0..KEYS {
            db.put(&key(i), &[(i % 251) as u8; 96]).unwrap();
        }
        db.compact_range(None, None).wait().unwrap();
        db.close().unwrap();
        dir
    };
    let dir = prepare(1024);
    let small_dir = prepare(256);

    let exe = std::env::current_exe().expect("test binary");
    struct Row {
        heap: i64,
        usage: i64,
        capacity: i64,
        adds: i64,
    }
    let run_rounds = |data: &std::path::Path, cache_bytes: usize, rounds: u32| -> Row {
        let out = std::process::Command::new(&exe)
            .args(["--exact", "adv_overhead_child", "--nocapture"])
            .env(DIR_ENV, data)
            .env(CACHE_ENV, cache_bytes.to_string())
            .env(ROUNDS_ENV, rounds.to_string())
            .output()
            .expect("spawn child");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "child with cache={cache_bytes} failed:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = stdout
            .lines()
            // Anywhere in the line: with one test thread libtest prints
            // "test <name> ... " in front of what the child prints.
            .find_map(|l| l.find("ADVRESULT").map(|at| &l[at..]))
            .unwrap_or_else(|| panic!("no result from child:\n{stdout}"))
            .to_string();
        println!("{line}");
        let field = |name: &str| -> i64 {
            line.split_whitespace()
                .find_map(|f| f.strip_prefix(name))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("missing {name} in {line}"))
        };
        Row {
            heap: field("heap="),
            usage: field("usage="),
            capacity: field("capacity="),
            adds: field("adds="),
        }
    };
    let run_in = |data: &std::path::Path, cache_bytes: usize| run_rounds(data, cache_bytes, 2);
    let run = |cache_bytes: usize| run_in(dir.path(), cache_bytes);

    // Baseline: the same reads with no cache at all. A disabled cache
    // must cost nothing on the read path.
    let off = run(0);
    assert_eq!(off.usage, 0, "a zero budget cached bytes");
    assert!(
        off.heap.abs() < 64 * 1024,
        "a disabled cache grew the heap by {} bytes over a read workload",
        off.heap
    );

    // Roomy: everything the workload touches fits, so nothing is
    // evicted and the delta is the pure per-entry cost.
    let roomy = run(64 * 1024 * 1024);
    assert!(
        roomy.adds > 0 && roomy.usage > 0,
        "the workload never filled the cache"
    );
    let roomy_real = roomy.heap - off.heap;
    let roomy_excess = roomy_real - roomy.usage;
    println!(
        "ADVOVERHEAD roomy real={roomy_real} charged={} excess={roomy_excess} \
entries={} excess_per_entry={:.2} charged_per_entry={:.1}",
        roomy.usage,
        roomy.adds,
        roomy_excess as f64 / roomy.adds as f64,
        roomy.usage as f64 / roomy.adds as f64
    );

    // Saturated: the hand runs constantly, so this also carries
    // whatever the cache's map has retired but not yet reclaimed.
    let tight = run(2 * 1024 * 1024);
    let tight_real = tight.heap - off.heap;
    println!(
        "ADVOVERHEAD saturated real={tight_real} charged={} capacity={} adds={} \
over_capacity={} ratio={:.4}",
        tight.usage,
        tight.capacity,
        tight.adds,
        tight_real - tight.capacity,
        tight_real as f64 / tight.capacity as f64
    );

    // Worst case for the charge: the smallest blocks the workload can
    // produce, so per-entry bookkeeping is the largest share of each
    // entry and any under-charge shows up as the largest fraction of
    // the budget.
    let tiny_off = run_in(small_dir.path(), 0);
    let tiny = run_in(small_dir.path(), 2 * 1024 * 1024);
    let tiny_real = tiny.heap - tiny_off.heap;
    println!(
        "ADVOVERHEAD small_blocks real={tiny_real} charged={} capacity={} adds={} \
charged_per_entry={:.1} over_capacity={} ratio={:.4}",
        tiny.usage,
        tiny.capacity,
        tiny.adds,
        tiny.usage as f64 / tiny.adds.max(1) as f64,
        tiny_real - tiny.capacity,
        tiny_real as f64 / tiny.capacity as f64
    );

    assert!(
        roomy_real <= roomy.usage + roomy.adds * 16,
        "an unevicted entry costs {} heap bytes more than the charge it is charged",
        roomy_excess as f64 / roomy.adds as f64
    );
    // The margin a saturated cache carries over its byte budget: the
    // bookkeeping `ENTRY_OVERHEAD` charges but `usage()` reports outside
    // the block bytes. The child drains the map's deferred reclamation
    // before it measures, so a retired node not yet freed does not count
    // here; the flatness check below is what guards that.
    //
    // The margin is per ENTRY, not per byte, so it is set by how many
    // entries a budget holds and therefore by the block size. These two
    // arms bracket it from the wrong end deliberately: 1 KiB blocks are
    // a quarter of the default `block_size` and 256-byte blocks a
    // sixteenth, so a default-configured cache sits well inside the
    // tighter of the two. Measured 1.03x at 1 KiB and 1.07x at 256
    // bytes, against 3.8x and rising if the reclamation is left
    // undrained.
    for (label, real, capacity, ceiling) in [
        ("1 KiB blocks", tight_real, tight.capacity, 18),
        ("256 B blocks", tiny_real, tiny.capacity, 24),
    ] {
        assert!(
            real <= capacity * ceiling / 10,
            "a saturated cache with {label} holds {real} heap bytes against a {capacity}-byte              budget, past the {:.1}x this configuration is allowed",
            ceiling as f64 / 10.0
        );
    }

    // The margin has to be a constant, not a leak. Tripling the churn
    // through the same budget must not move resident memory much: if it
    // does, the drain that reclaims evicted map nodes has stopped
    // keeping up, and the byte budget bounds only what the cache
    // accounts rather than what it holds. Undrained that measures 144
    // bytes retained per insert: 7.9 MB after 40,000 inserts, 19.5 MB
    // after 120,000 and 71.3 MB after 480,000, with no ceiling.
    let churned = run_rounds(small_dir.path(), 2 * 1024 * 1024, 6);
    let churned_real = churned.heap - tiny_off.heap;
    println!(
        "ADVOVERHEAD churn_flat rounds=2 real={tiny_real} adds={} | rounds=6 real={churned_real} adds={} ratio={:.4}",
        tiny.adds,
        churned.adds,
        churned_real as f64 / tiny_real as f64
    );
    assert!(
        churned.adds > tiny.adds,
        "the longer run cached no more entries than the short one, so it is not more churn:          {} against {}",
        churned.adds,
        tiny.adds
    );
    let extra = churned_real.saturating_sub(tiny_real);
    let per_add = extra as f64 / (churned.adds - tiny.adds).max(1) as f64;
    println!("ADVOVERHEAD retained_per_insert={per_add:.1} bytes");
    // The rate, not exact flatness. The child drains kovan before each
    // measurement and runs no compaction worker, so the figure does not
    // move with machine load: 0.0 bytes per insert idle and with
    // twice as many busy loops as cores. Undrained it is 144, so a ceiling
    // of 40 still fails by a wide margin if the drain stops working.
    assert!(
        per_add <= 40.0,
        "the cache retains {per_add:.1} bytes per insert, so resident memory grows with churn \
         rather than with the working set. Undrained this measures 144 bytes per insert, and a \
         working drain measures under 1"
    );
}
