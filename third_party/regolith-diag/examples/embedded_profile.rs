//! Measure regolith's memory high-water mark through a full database
//! lifecycle, so the 1-4 MiB embedded budget is a number somebody ran
//! rather than a number somebody hoped for.
//!
//! Two host measurements, because the two targets fail differently:
//!
//! - **Linux**: resident set size from `/proc/self/statm`, plus the
//!   kernel's own peak (`VmHWM` from `/proc/self/status`). Linux
//!   overcommits and only bills pages actually touched, so RSS is the
//!   honest figure for a Linux-class device.
//! - **wasm**: `memory_size(0) * 65536`, the size of linear memory.
//!   Linear memory grows in 64 KiB pages and **never shrinks**, so
//!   every high-water mark is permanent for the life of the instance.
//!   This is why the same code costs far more on wasm than its Linux
//!   RSS suggests.
//!
//! Everything else prints as `unavailable`, which is the truth on a
//! host this example cannot probe, rather than a zero that would read
//! as "free".
//!
//! # Running it
//!
//! ```sh
//! cargo run --release --example embedded_profile -- /tmp/regolith-mem
//! cargo run --release --example embedded_profile -- /tmp/regolith-mem wasm
//! cargo run --release --example embedded_profile -- /tmp/regolith-mem default
//!
//! cargo build --release --example embedded_profile --target wasm32-wasip1
//! wasmtime run --dir=/tmp/regolith-mem::/data \
//!     target/wasm32-wasip1/release/examples/embedded_profile.wasm /data
//! ```
//!
//! Arguments: `<db-dir> [embedded|wasm|default] [num-puts]`. The directory
//! must already exist on wasip1, where it is a host preopen.

use std::path::Path;

use regolith::{Db, Options};

/// Value size, in bytes, for every write this example makes. Matches
/// the 128-byte payload the project's earlier embedded measurements
/// used, so the numbers are comparable.
const VALUE_SIZE: usize = 128;

/// Default number of writes in the bulk phase.
const DEFAULT_PUTS: usize = 20_000;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dir = args
        .first()
        .map(String::as_str)
        .ok_or("usage: embedded_profile <db-dir> [embedded|wasm|default] [num-puts]")?;
    let profile = args.get(1).map(String::as_str).unwrap_or("embedded");
    let puts: usize = match args.get(2) {
        Some(n) => n.parse()?,
        None => DEFAULT_PUTS,
    };

    let opts = match profile {
        "embedded" => Options::embedded(),
        "wasm" => Options::wasm(),
        "default" => Options::default(),
        other => {
            return Err(format!("unknown profile {other:?}; use embedded, wasm or default").into());
        }
    };

    println!("regolith memory profile");
    println!("  probe          {}", Probe::describe());
    println!("  profile        {profile}");
    println!("  writes         {puts} x {VALUE_SIZE} B values, 16 B keys");
    println!(
        "  write_buffer   {} KiB",
        opts.get_write_buffer_size() / 1024
    );
    println!(
        "  block_cache    {} KiB",
        opts.get_block_cache_size() / 1024
    );
    println!(
        "  target_file    {} KiB",
        opts.get_target_file_size() / 1024
    );
    println!("  bg_compactions {}", opts.get_max_background_compactions());
    println!();

    let mut report = Report::new();
    report.record("baseline (before open)");

    let db_dir = Path::new(dir).join("db");
    let db = Db::open(&db_dir, opts.clone())?;
    report.record("after Db::open");

    let mut value = vec![0u8; VALUE_SIZE];
    for i in 0..puts {
        // Vary the payload so compression cannot collapse the whole
        // database into nothing and flatter the measurement.
        let tag = (i as u64).to_le_bytes();
        value[..8].copy_from_slice(&tag);
        db.put(&key(i), &value)?;
        if i + 1 == 1_000 {
            report.record("after 1k puts");
        }
    }
    report.record(&format!("after {puts} puts"));

    // A read pass over what is now on disk: with no block cache this
    // is the phase that shows what an uncached read path costs.
    let mut found = 0usize;
    for i in (0..puts).step_by(7) {
        if db.get(&key(i))?.is_some() {
            found += 1;
        }
    }
    report.record("after sampled reads");

    // Bounded on purpose: `Db::scan` materializes a whole range,
    // which is the one unbounded read-set allocation in the API.
    let scanned = db.scan_page(Some(&key(0)), None, 512)?.entries.len();
    report.record("after a 512-row page scan");

    db.compact_range(None, None).wait()?;
    report.record("after compact_range");

    db.close()?;
    drop(db);
    report.record("after close (flush to SSTable)");

    let reopened = Db::open(&db_dir, opts)?;
    report.record("after reopen");
    let probe_key = key(puts / 2);
    let round_trip = reopened.get(&probe_key)?;
    reopened.close()?;
    drop(reopened);
    report.record("after second close");

    println!("{report}");
    println!();
    println!("  sampled reads found  {found}");
    println!("  scan returned        {scanned} rows");
    let round_trip_ok = round_trip.as_ref().is_some_and(|v| v.len() == VALUE_SIZE);
    println!(
        "  round-trip after reopen  {}",
        match (&round_trip, round_trip_ok) {
            (_, true) => "ok",
            (Some(_), false) => "WRONG LENGTH",
            (None, _) => "MISSING",
        }
    );

    if round_trip_ok {
        Ok(())
    } else {
        Err("lifecycle failed: the key written before close did not survive reopen".into())
    }
}

/// 16-byte key, ordered so sequential `i` produces sequential keys.
fn key(i: usize) -> Vec<u8> {
    format!("key{i:013}").into_bytes()
}

/// One phase of the lifecycle and what memory looked like at it.
struct Phase {
    label: String,
    sample: Sample,
}

/// A collected run, printed as a table at the end so the phases are
/// read side by side rather than interleaved with progress output.
struct Report {
    phases: Vec<Phase>,
    baseline: Option<Sample>,
}

impl Report {
    fn new() -> Self {
        Self {
            phases: Vec::new(),
            baseline: None,
        }
    }

    fn record(&mut self, label: &str) {
        let sample = Probe::sample();
        if self.baseline.is_none() {
            self.baseline = Some(sample);
        }
        self.phases.push(Phase {
            label: label.to_string(),
            sample,
        });
    }
}

impl std::fmt::Display for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "  {:<32} {:>12} {:>12} {:>12}",
            "phase", "current", "delta", "peak"
        )?;
        writeln!(f, "  {}", "-".repeat(70))?;
        let base = self.baseline.and_then(|s| s.current_bytes);
        for phase in &self.phases {
            let delta = match (base, phase.sample.current_bytes) {
                (Some(b), Some(c)) => format!("{:+}", (c as i64 - b as i64) / 1024),
                _ => "-".to_string(),
            };
            writeln!(
                f,
                "  {:<32} {:>12} {:>12} {:>12}",
                phase.label,
                kib(phase.sample.current_bytes),
                delta,
                kib(phase.sample.peak_bytes),
            )?;
        }
        write!(f, "  all figures in KiB")
    }
}

fn kib(bytes: Option<u64>) -> String {
    match bytes {
        Some(b) => (b / 1024).to_string(),
        None => "unavailable".to_string(),
    }
}

/// One memory reading. `None` means this host does not expose that
/// figure; it is never reported as zero, because zero would read as
/// "costs nothing".
#[derive(Clone, Copy)]
struct Sample {
    current_bytes: Option<u64>,
    peak_bytes: Option<u64>,
}

struct Probe;

#[cfg(target_family = "wasm")]
impl Probe {
    fn describe() -> &'static str {
        "wasm linear memory (memory_size * 64 KiB; never shrinks)"
    }

    fn sample() -> Sample {
        // Linear memory is monotonic, so the current size is also the
        // high-water mark by construction.
        let bytes = (core::arch::wasm32::memory_size(0) as u64) * 65536;
        Sample {
            current_bytes: Some(bytes),
            peak_bytes: Some(bytes),
        }
    }
}

#[cfg(all(target_os = "linux", not(target_family = "wasm")))]
impl Probe {
    fn describe() -> &'static str {
        "Linux RSS (/proc/self/statm) and peak RSS (VmHWM)"
    }

    fn sample() -> Sample {
        Sample {
            current_bytes: rss_bytes(),
            peak_bytes: vm_hwm_bytes(),
        }
    }
}

#[cfg(all(target_os = "linux", not(target_family = "wasm")))]
fn rss_bytes() -> Option<u64> {
    // statm field 2 is resident pages. The page size is 4 KiB on every
    // target this example is built for; a host with a different page
    // size would need `sysconf(_SC_PAGESIZE)`, which needs libc.
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

#[cfg(all(target_os = "linux", not(target_family = "wasm")))]
fn vm_hwm_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmHWM:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

#[cfg(not(any(target_family = "wasm", target_os = "linux")))]
impl Probe {
    fn describe() -> &'static str {
        "no memory probe on this host"
    }

    fn sample() -> Sample {
        Sample {
            current_bytes: None,
            peak_bytes: None,
        }
    }
}
