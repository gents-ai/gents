//! Large-value bench: put and get at 1, 16 and 64 MiB, with process memory
//! recorded around each size.
//!
//! 64 MiB is the default `max_value_size`, and the engine accepts a value of
//! exactly that length, so the standard sweep runs right up to the limit
//! without reconfiguring anything. The 1 GiB case has to raise the limit, and
//! costs multiples of the value in resident memory, so it is gated behind
//! `REGOLITH_BENCH_HUGE=1` and skipped by default.
//!
//! `VmHWM` is a process high-water mark that never decreases, so the peak
//! reported for one size includes every earlier size. Sizes therefore run in
//! ascending order, and the growth from one row to the next is the part
//! attributable to that row. Both memory figures come from `/proc` and are
//! reported as `null` on platforms that do not expose it.

mod common;

use std::time::Instant;

const MIB: usize = 1024 * 1024;
const SIZES_MIB: [usize; 3] = [1, 16, 64];
const HUGE_MIB: usize = 1024;
const PROCESS_METRICS: bool = cfg!(target_os = "linux");

struct SizeResult {
    mib: usize,
    put_secs: Vec<f64>,
    get_secs: Vec<f64>,
    rss_before_kib: u64,
    rss_after_kib: u64,
    peak_after_kib: u64,
}

impl SizeResult {
    fn json(&mut self) -> String {
        let put = common::median(&mut self.put_secs);
        let get = common::median(&mut self.get_secs);
        let mib = self.mib as f64;
        format!(
            "{{\"value_mib\":{},\"reps\":{},\
             \"put_secs_median\":{put:.4},\"put_mib_per_s\":{:.1},\
             \"get_secs_median\":{get:.4},\"get_mib_per_s\":{:.1},\
             \"rss_before_kib\":{},\"rss_after_kib\":{},\"peak_rss_after_kib\":{}}}",
            self.mib,
            self.put_secs.len(),
            mib / put,
            mib / get,
            metric(self.rss_before_kib),
            metric(self.rss_after_kib),
            metric(self.peak_after_kib),
        )
    }
}

/// Process memory is Linux-only. Reporting the 0 from another platform as a
/// measurement would be a fabricated number, so it goes out as JSON null.
fn metric(kib: u64) -> String {
    if PROCESS_METRICS {
        kib.to_string()
    } else {
        "null".to_string()
    }
}

fn secs(started: Instant) -> f64 {
    let s = started.elapsed().as_secs_f64();
    assert!(s > 0.0, "timed op reported a zero duration");
    s
}

fn run_size(mib: usize, reps: usize, opts: regolith::Options) -> SizeResult {
    let (_tmp, db) = common::open(&format!("large-value-{mib}mib"), opts);
    let mut rng = common::Rng::new(0x1A26_0000 ^ mib as u64);
    let value = common::rand_value(&mut rng, mib * MIB);

    let rss_before_kib = common::rss_kib();
    let mut put_secs = Vec::with_capacity(reps);
    let mut get_secs = Vec::with_capacity(reps);
    for rep in 0..reps {
        let key = common::key(rep as u64);

        let started = Instant::now();
        db.put(&key, &value)
            .unwrap_or_else(|e| panic!("put {mib} MiB value: {e}"));
        put_secs.push(secs(started));

        let started = Instant::now();
        let got = db
            .get(&key)
            .unwrap_or_else(|e| panic!("get {mib} MiB value: {e}"));
        get_secs.push(secs(started));

        let got = got.unwrap_or_else(|| panic!("{mib} MiB value missing after put"));
        assert!(
            got == value,
            "{mib} MiB value did not round-trip: {} bytes back",
            got.len()
        );
    }

    SizeResult {
        mib,
        put_secs,
        get_secs,
        rss_before_kib,
        rss_after_kib: common::rss_kib(),
        peak_after_kib: common::peak_rss_kib(),
    }
}

fn report(result: &mut SizeResult) -> String {
    let json = result.json();
    let mib = result.mib as f64;
    let put = common::median(&mut result.put_secs);
    let get = common::median(&mut result.get_secs);
    let memory = if PROCESS_METRICS {
        format!(
            "rss {} -> {} KiB, peak {} KiB",
            result.rss_before_kib, result.rss_after_kib, result.peak_after_kib
        )
    } else {
        "rss/peak not available on this platform".to_string()
    };
    println!(
        "{:>5} MiB  put {put:.4} s ({:.0} MiB/s)  get {get:.4} s ({:.0} MiB/s)  {memory}",
        result.mib,
        mib / put,
        mib / get
    );
    json
}

fn huge_requested() -> bool {
    std::env::var("REGOLITH_BENCH_HUGE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

fn main() {
    let quick = std::env::args().any(|a| a == "--quick" || a == "--test");
    let reps = if quick { 1 } else { 3 };

    println!("large_value bench: default max_value_size is 64 MiB, the sweep runs up to it");
    let mut sizes = Vec::with_capacity(SIZES_MIB.len());
    for mib in SIZES_MIB {
        let mut result = run_size(mib, reps, common::default_opts());
        sizes.push(report(&mut result));
    }

    let gib_case = if huge_requested() {
        println!(
            "REGOLITH_BENCH_HUGE=1: running the {HUGE_MIB} MiB case with max_value_size raised"
        );
        let opts = common::default_opts().max_value_size(HUGE_MIB * MIB);
        let mut result = run_size(HUGE_MIB, 1, opts);
        report(&mut result)
    } else {
        println!("{HUGE_MIB} MiB case skipped: set REGOLITH_BENCH_HUGE=1 to run it");
        "null".to_string()
    };

    common::write_family(
        "large_value",
        &format!(
            "{{\"quick\":{quick},\"process_metrics_available\":{PROCESS_METRICS},\
             \"compression\":\"{:?}\",\"default_max_value_mib\":{},\
             \"sizes\":[{}],\"gib_case\":{gib_case},\
             \"gib_note\":\"the {HUGE_MIB} MiB case raises max_value_size and runs only with REGOLITH_BENCH_HUGE=1\"}}",
            common::default_opts().get_compression(),
            common::default_opts().get_max_value_size() / MIB,
            sizes.join(",")
        ),
    );
}
