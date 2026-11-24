//! The block cache's own footprint, measured in a fresh process: it must be
//! driven by the byte budget, never by the shard count.
//!
//! The defect this guards cost 254,656 KiB for an 8 MiB configured
//! budget because every shard preallocated a fixed table. A single
//! in-process loop cannot see that: allocator reuse and page
//! granularity hide it. Each shard count therefore gets its own
//! process, and the measurement is a counting global allocator (exact,
//! load-independent) with `VmRSS` reported alongside it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};

use regolith::{Db, Options};
use tempfile::TempDir;

static LIVE: AtomicIsize = AtomicIsize::new(0);

struct Counting;

// SAFETY: every method forwards to `System`, which is a correct
// allocator, and only adds relaxed counter updates around it. No
// pointer or layout is altered, so the `GlobalAlloc` contract is
// exactly `System`'s.
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

const BUDGET: usize = 8 * 1024 * 1024;
const SHARD_BITS: [u32; 5] = [0, 2, 4, 6, 8];
const CHILD_ENV: &str = "REGOLITH_ADV_FOOTPRINT_BITS";

fn vm_rss_kib() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:")
            && let Some(kib) = rest.split_whitespace().next()
        {
            return kib.parse().unwrap_or(0);
        }
    }
    0
}

/// Runs in the child process: open an empty DB at one shard count and
/// print what it cost. A no-op in the parent.
#[test]
fn adv_footprint_child() {
    let Ok(bits) = std::env::var(CHILD_ENV) else {
        return;
    };
    let bits: u32 = bits.parse().expect("shard bits");
    let dir = TempDir::new().unwrap();

    let rss_before = vm_rss_kib();
    let heap_before = LIVE.load(Ordering::Relaxed);
    let db = Db::open(
        dir.path(),
        Options::default()
            .block_cache_size(BUDGET)
            .block_cache_num_shard_bits(bits),
    )
    .unwrap();
    let heap_after = LIVE.load(Ordering::Relaxed);
    let rss_after = vm_rss_kib();

    let capacity = db
        .get_int_property("regolith.block-cache-capacity")
        .unwrap();
    let usage = db.get_int_property("regolith.block-cache-usage").unwrap();
    assert_eq!(usage, 0, "a freshly opened DB holds cached bytes");
    println!(
        "ADVRESULT bits={bits} heap={} rss_kib={} capacity={capacity} usage={usage}",
        heap_after - heap_before,
        rss_after.saturating_sub(rss_before)
    );
    drop(db);
}

#[test]
fn empty_db_footprint_is_flat_in_shard_count() {
    if std::env::var(CHILD_ENV).is_ok() {
        return;
    }
    let exe = std::env::current_exe().expect("test binary");
    let mut rows = Vec::new();
    for bits in SHARD_BITS {
        let out = std::process::Command::new(&exe)
            .args(["--exact", "adv_footprint_child", "--nocapture"])
            .env(CHILD_ENV, bits.to_string())
            .output()
            .expect("spawn child");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "child at shard_bits {bits} failed:\n{stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let line = stdout
            .lines()
            // Anywhere in the line: with one test thread libtest prints
            // "test <name> ... " in front of what the child prints.
            .find_map(|l| l.find("ADVRESULT").map(|at| &l[at..]))
            .unwrap_or_else(|| panic!("child at shard_bits {bits} printed no result:\n{stdout}"))
            .to_string();
        let field = |name: &str| -> i64 {
            line.split_whitespace()
                .find_map(|f| f.strip_prefix(name))
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("missing {name} in {line}"))
        };
        rows.push((bits, field("heap="), field("rss_kib="), field("capacity=")));
        println!("{line}");
    }

    let heaps: Vec<i64> = rows.iter().map(|r| r.1).collect();
    let base = heaps[0];
    for (bits, heap, _, _) in &rows {
        assert!(
            *heap <= base + 64 * 1024,
            "shard_bits {bits} costs {heap} heap bytes against {base} at one shard: \
             the cache's footprint tracks the shard count"
        );
        assert!(
            *heap < BUDGET as i64 / 8,
            "shard_bits {bits} costs {heap} heap bytes on an empty {BUDGET}-byte cache"
        );
    }
    for (bits, _, _, capacity) in &rows {
        assert!(
            *capacity <= BUDGET as i64,
            "shard_bits {bits} reports capacity {capacity} over the {BUDGET}-byte budget"
        );
    }
}
