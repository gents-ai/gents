//! What a compaction pass costs as dropped column families pile up (E27).
//!
//! Each cycle creates a column family, writes a few keys to it, drops it,
//! writes to the default family, and runs `compact_range(None, None)`, which
//! is the pass timed. The pass's inputs span from the default family to the
//! newest dropped one, so at the bottom level it meets the tables holding
//! every earlier drop's range tombstone, unless those were retired. Reported:
//! the median pass time in windows of cycles, early and late, and the bytes
//! the tables hold at the end.
//!
//! `--quick` (or `--test`) runs fewer cycles.

mod common;

use std::time::Instant;

fn main() {
    let quick = common::args()
        .iter()
        .any(|a| a == "--quick" || a == "--test");
    let (cycles, window) = if quick { (64, 16) } else { (512, 64) };

    let (tmp, db) = common::open(
        "cf-drop",
        common::default_opts().max_background_compactions(0),
    );
    let mut pass_us = Vec::with_capacity(cycles);
    for cycle in 0..cycles {
        let cf = db
            .create_column_family(&format!("cf{cycle}"))
            .unwrap_or_else(|e| panic!("create: {e}"));
        for i in 0..8u32 {
            db.put_cf(&cf, format!("k{i}").as_bytes(), b"dropped")
                .unwrap_or_else(|e| panic!("put_cf: {e}"));
        }
        db.drop_column_family(cf)
            .unwrap_or_else(|e| panic!("drop: {e}"));
        for i in 0..64u32 {
            db.put(&common::key(i as u64), format!("v{cycle}").as_bytes())
                .unwrap_or_else(|e| panic!("put: {e}"));
        }
        let started = Instant::now();
        db.compact_range(None, None)
            .wait()
            .unwrap_or_else(|e| panic!("compact: {e}"));
        pass_us.push(started.elapsed().as_secs_f64() * 1e6);
    }

    let windows = [
        (0, window),
        (cycles / 2 - window / 2, cycles / 2 + window / 2),
        (cycles - window, cycles),
    ];
    println!("cf drop: {cycles} create/write/drop cycles, one compact_range(None, None) each");
    let mut rows = Vec::new();
    for (from, to) in windows {
        let mut sample = pass_us[from..to].to_vec();
        let median = common::median(&mut sample);
        println!("  cycles {from:>4}..{to:<4} pass median {median:>10.1} us");
        rows.push(format!(
            "{{\"from\":{from},\"to\":{to},\"pass_median_us\":{median:.1}}}"
        ));
    }
    let bytes = db
        .get_int_property("regolith.total-sst-files-size")
        .unwrap_or(0);
    println!("  table bytes at the end {bytes}");
    common::write_family(
        "cf_drop",
        &format!(
            "{{\"cycles\":{cycles},\"windows\":[{}],\"table_bytes\":{bytes}}}",
            rows.join(",")
        ),
    );
    db.close().unwrap_or_else(|e| panic!("close: {e}"));
    drop(tmp);
}
