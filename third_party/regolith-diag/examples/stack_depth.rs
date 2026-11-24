//! Measure how much stack each regolith code path actually consumes.
//!
//! This matters because an embedded stack is commonly 4 to 8 KiB in
//! total. A frame that is too fat there is a hard crash on a guard
//! page, not a slowdown, and the project has never had a number for
//! it. Anything above 4 KiB is reported as a FINDING.
//!
//! # Method
//!
//! Stack painting, with the frame gaps cancelled out:
//!
//! 1. Recurse to a known depth, writing a byte pattern into a buffer
//!    in every frame, and return. The region below the caller's frame
//!    is now painted, except for the per-frame overhead (return
//!    address, saved registers) that the recursion could not write.
//! 2. Snapshot that region. This records where the un-paintable gaps
//!    are, so the next step does not have to guess.
//! 3. Repaint identically, run the workload, snapshot again.
//! 4. The lowest address whose byte differs between the two snapshots
//!    is how deep the workload went. The gaps appear identically in
//!    both snapshots and cancel, which is what makes this exact
//!    rather than accurate to one paint chunk.
//!
//! A calibration pass measures a workload with a known 64 KiB frame
//! first. Its excess over 64 KiB is the harness overhead, and it is
//! printed rather than silently subtracted.
//!
//! # What the numbers are not
//!
//! They are host figures. Frame layout is target-specific, so a value
//! measured on x86_64 does not transfer to ARM or RISC-V. Re-run this
//! on the target before treating any of it as a budget. The workload
//! can also coincidentally write the pattern byte at its deepest
//! point, which would under-report by a few bytes.
//!
//! # Running it
//!
//! ```sh
//! cargo run --release --example stack_depth -- /tmp/regolith-stack
//! ```
//!
//! Release matters: an unoptimized build has far larger frames and
//! measures the debug profile, not the shipped one.

use std::path::{Path, PathBuf};

use regolith::{Db, Options};

/// Byte written into every painted stack slot. Not zero, because
/// zeroed stack is common and would collide with genuine writes.
const PATTERN: u8 = 0xC5;

/// Bytes each recursion level paints.
const PAINT_CHUNK: usize = 1024;

/// Recursion depth, so the painted window is 128 KiB. Anything deeper
/// than that reports as saturated instead of as a small number.
const PAINT_LEVELS: usize = 128;

/// Stack for the measuring thread. Must comfortably exceed the
/// painted window plus whatever the workload itself needs.
const MEASURE_THREAD_STACK: usize = 8 * 1024 * 1024;

/// The threshold the task cares about: a common embedded stack.
const FINDING_THRESHOLD: usize = 4 * 1024;

/// Bytes the calibration workload puts on the stack.
const CALIBRATION_BYTES: usize = 64 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir: PathBuf = std::env::args()
        .nth(1)
        .ok_or("usage: stack_depth <scratch-dir>")?
        .into();
    std::fs::create_dir_all(&dir)?;
    let db_dir = dir.join("db");

    if cfg!(debug_assertions) {
        println!("WARNING: debug build. Frames are much larger than the shipped");
        println!("         release profile. Re-run with --release.\n");
    }

    build_fixture(&db_dir)?;

    // A dedicated thread, so the painted window is known-clean rather
    // than sharing whatever the runtime already left on the main stack.
    let handle = std::thread::Builder::new()
        .name("regolith-stack-probe".to_string())
        .stack_size(MEASURE_THREAD_STACK)
        .spawn(move || run_probes(&db_dir))?;
    let results = handle.join().map_err(|_| "measuring thread panicked")??;

    println!("regolith stack depth, {} profile", profile_name());
    println!(
        "  method     paint 0x{PATTERN:02X}, {PAINT_LEVELS} x {PAINT_CHUNK} B window, gap-cancelled diff"
    );
    println!("  build      {}", build_profile());
    println!(
        "  host       {} / {}",
        std::env::consts::ARCH,
        std::env::consts::OS
    );
    println!();
    // The harness has a fixed cost, established by the calibration
    // pass. It is subtracted into a separate column rather than folded
    // into the raw reading, so both numbers stay visible.
    let overhead = results
        .iter()
        .find(|r| r.is_calibration)
        .map(|c| c.bytes.saturating_sub(CALIBRATION_BYTES))
        .unwrap_or(0);

    println!("  {:<40} {:>8} {:>8}  verdict", "path", "raw", "net");
    println!("  {}", "-".repeat(76));

    let mut findings = 0usize;
    let mut measured = 0usize;
    for r in &results {
        let net = r.bytes.saturating_sub(overhead);
        let verdict = if r.is_calibration {
            format!("harness overhead {overhead} B")
        } else {
            measured += 1;
            if r.saturated {
                format!("SATURATED (> {} B window)", PAINT_LEVELS * PAINT_CHUNK)
            } else if net > FINDING_THRESHOLD {
                findings += 1;
                format!("FINDING: over {FINDING_THRESHOLD} B")
            } else {
                "within 4 KiB".to_string()
            }
        };
        println!("  {:<40} {:>8} {:>8}  {}", r.label, r.bytes, net, verdict);
    }

    println!();
    let peak = results
        .iter()
        .filter(|r| !r.is_calibration)
        .map(|r| r.bytes.saturating_sub(overhead))
        .max();
    match peak {
        Some(p) => println!(
            "  peak across measured paths   {p} B ({:.1} KiB)",
            p as f64 / 1024.0
        ),
        None => println!("  peak across measured paths   not measured"),
    }
    println!("  paths over {FINDING_THRESHOLD} B          {findings} of {measured}");
    println!(
        "  calibration                  measured {} B for a known {CALIBRATION_BYTES} B frame",
        results
            .iter()
            .find(|r| r.is_calibration)
            .map(|c| c.bytes)
            .unwrap_or(0)
    );

    Ok(())
}

fn profile_name() -> &'static str {
    "embedded"
}

fn build_profile() -> &'static str {
    if cfg!(debug_assertions) {
        "debug (numbers not representative)"
    } else {
        "release"
    }
}

/// Build a database with a real level structure: several L0 files that
/// overlap, deeper levels underneath them, and unflushed writes so
/// `Db::open` has a WAL to replay. Measuring `open` against an empty
/// directory would measure nothing interesting.
fn build_fixture(db_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _ = std::fs::remove_dir_all(db_dir);
    let db = Db::open(db_dir, Options::embedded())?;

    let mut value = vec![0u8; 128];
    // Enough to cross many memtable rotations at a 256 KiB write
    // buffer, then compact so the deeper levels are populated.
    for i in 0..40_000u64 {
        value[..8].copy_from_slice(&i.to_le_bytes());
        db.put(&key(i), &value)?;
    }
    db.compact_range(None, None).wait()?;

    // Now lay fresh, overlapping L0 files on top of the compacted
    // base, so a point read has to consult L0 before reaching down.
    for i in (0..40_000u64).step_by(3) {
        value[..8].copy_from_slice(&i.to_le_bytes());
        db.put(&key(i), &value)?;
    }
    // Leave these unflushed: they become WAL replay work at open.
    for i in 0..200u64 {
        value[..8].copy_from_slice(&i.to_le_bytes());
        db.put(&key(i), &value)?;
    }

    println!("fixture: {}", level_shape(&db));
    db.close()?;
    Ok(())
}

fn level_shape(db: &Db) -> String {
    let mut parts = Vec::new();
    for level in 0..7 {
        let name = format!("regolith.num-files-at-level{level}");
        if let Some(n) = db.get_int_property(&name)
            && n > 0
        {
            parts.push(format!("L{level}={n}"));
        }
    }
    if parts.is_empty() {
        "no SSTables".to_string()
    } else {
        parts.join(" ")
    }
}

fn key(i: u64) -> Vec<u8> {
    format!("key{i:013}").into_bytes()
}

/// One measured path.
struct Probe {
    label: String,
    bytes: usize,
    saturated: bool,
    is_calibration: bool,
}

fn run_probes(db_dir: &Path) -> Result<Vec<Probe>, String> {
    let mut out = Vec::new();

    // Calibration first: a workload whose stack cost is known exactly.
    out.push(measure("calibration (known 64 KiB frame)", true, || {
        consume_64k();
    }));

    let mut opened: Option<Db> = None;
    out.push(measure(
        "Db::open (multi-level + WAL replay)",
        false,
        || {
            opened = Db::open(db_dir, Options::embedded()).ok();
        },
    ));
    let db = opened.ok_or_else(|| "Db::open failed inside the probe".to_string())?;
    println!("opened:  {}", level_shape(&db));

    let hit = key(20_001);
    let miss = key(9_999_999);
    out.push(measure("point read, key present", false, || {
        let _ = std::hint::black_box(db.get(std::hint::black_box(&hit)));
    }));
    out.push(measure(
        "point read, key absent (bloom miss)",
        false,
        || {
            let _ = std::hint::black_box(db.get(std::hint::black_box(&miss)));
        },
    ));

    let seek_target = key(12_345);
    out.push(measure("iterator seek + 50-entry walk", false, || {
        let mut it = db.iter();
        it.seek(std::hint::black_box(&seek_target));
        let mut n = 0;
        while it.valid() && n < 50 {
            let _ = std::hint::black_box(it.key());
            it.next();
            n += 1;
        }
    }));

    out.push(measure("scan_page of 1000 rows", false, || {
        let _ = std::hint::black_box(db.scan_page(None, None, 1000));
    }));

    let mut value = vec![7u8; 128];
    out.push(measure(
        "8000 puts crossing a flush boundary",
        false,
        || {
            for i in 100_000u64..108_000 {
                value[..8].copy_from_slice(&i.to_le_bytes());
                if db.put(&key(i), &value).is_err() {
                    return;
                }
            }
        },
    ));

    out.push(measure("compaction merge (compact_range)", false, || {
        let _ = std::hint::black_box(db.compact_range(None, None).wait());
    }));

    let _ = db.close();
    Ok(out)
}

/// A workload with a stack cost that is known by construction, used to
/// calibrate the harness rather than to learn anything about regolith.
#[inline(never)]
fn consume_64k() {
    let buf = [0u8; CALIBRATION_BYTES];
    std::hint::black_box(&buf);
}

/// Paint the stack below this frame and return the lowest address
/// written.
///
/// The recursion is deliberately not in tail position - `buf` is used
/// again after the recursive call - so the compiler cannot collapse
/// every level into one reused frame, which would paint 1 KiB instead
/// of 128 KiB and quietly invalidate every number here.
#[inline(never)]
fn paint(levels: usize) -> usize {
    let mut buf = [PATTERN; PAINT_CHUNK];
    let here = buf.as_mut_ptr() as usize;
    if levels <= 1 {
        std::hint::black_box(buf.as_mut_ptr());
        return here;
    }
    let deepest = paint(levels - 1);
    std::hint::black_box(buf.as_mut_ptr());
    deepest
}

/// Copy the painted window out so two runs can be compared byte for
/// byte.
///
/// SAFETY: `[low, top)` lies inside this thread's stack and `paint`
/// has just written to every address in it, so every byte is mapped
/// and readable. The frames that owned those bytes have returned, so
/// nothing else can be observing them; `read_volatile` keeps the
/// compiler from assuming the reads are dead. This is the one place
/// the measurement needs to look at memory Rust considers finished
/// with, which is why it lives in an example and not in the library -
/// `src/lib.rs` carries `#![forbid(unsafe_code)]`.
#[inline(never)]
fn snapshot(low: usize, top: usize) -> Vec<u8> {
    let len = top - low;
    let mut out = vec![0u8; len];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = unsafe { std::ptr::read_volatile((low + i) as *const u8) };
    }
    out
}

fn measure<F: FnOnce()>(label: &str, is_calibration: bool, workload: F) -> Probe {
    let anchor: u8 = 0;
    let top = std::hint::black_box(&anchor) as *const u8 as usize;

    let low = paint(PAINT_LEVELS);
    let before = snapshot(low, top);

    let low_again = paint(PAINT_LEVELS);
    // Both paints run from this frame at the same depth, so they must
    // land on the same addresses. If they ever did not, `before` and
    // `after` would describe different regions and the diff would be
    // meaningless, so this is checked rather than assumed.
    assert_eq!(
        low, low_again,
        "paint frames moved between runs; the measurement would be invalid"
    );

    workload();

    let after = snapshot(low, top);

    let first_diff = before
        .iter()
        .zip(after.iter())
        .position(|(b, a)| b != a)
        .unwrap_or(before.len());

    Probe {
        label: label.to_string(),
        bytes: top - (low + first_diff),
        saturated: first_diff == 0,
        is_calibration,
    }
}
