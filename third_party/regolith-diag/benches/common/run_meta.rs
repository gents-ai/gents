//! Provenance for a run file: who measured, on what, how quiet the host was,
//! and the plan targets a renderer draws when only one run exists.
//!
//! Included by collect.rs with `#[path]`, alongside `json` and `records` at the
//! crate root.

#![allow(dead_code)]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::json::{self, Json};

pub struct Guard {
    pub passed: bool,
    load: Option<(f64, f64, f64)>,
    note: String,
}

impl Guard {
    pub fn json(&self) -> Json {
        let (l1, l5, l15) = match self.load {
            Some((a, b, c)) => (Json::Num(a), Json::Num(b), Json::Num(c)),
            None => (Json::Null, Json::Null, Json::Null),
        };
        Json::obj(vec![
            ("passed", Json::Bool(self.passed)),
            ("loadavg_1m", l1),
            ("loadavg_5m", l5),
            ("loadavg_15m", l15),
            ("note", Json::Str(self.note.clone())),
        ])
    }
}

/// Fails closed. If the guard cannot be run the host is not certified quiet,
/// so throughput is marked contaminated with the reason rather than presumed
/// clean.
pub fn loadguard(out: &Path) -> Guard {
    let load = loadavg();
    let script = match std::env::var_os("REGOLITH_LOADGUARD") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        // `just gains` writes into <gains dir>/runs/<file>, and loadguard.py
        // sits at the top of that directory.
        _ => match out.parent().and_then(|p| p.parent()) {
            Some(d) => d.join("loadguard.py"),
            None => PathBuf::from("loadguard.py"),
        },
    };
    if !script.is_file() {
        return Guard {
            passed: false,
            load,
            note: format!(
                "loadguard.py not found at {} (set REGOLITH_LOADGUARD to point at it). Not run, so \
                 the host is not certified quiet: throughput is marked contaminated rather than \
                 presumed clean.",
                script.display()
            ),
        };
    }
    match Command::new("python3").arg(&script).output() {
        Err(e) => Guard {
            passed: false,
            load,
            note: format!(
                "could not run {}: {e}. The host is not certified quiet, so throughput is marked \
                 contaminated rather than presumed clean.",
                script.display()
            ),
        },
        Ok(o) => {
            let passed = o.status.success();
            let note = if passed {
                format!(
                    "{} exited 0: the host was quiet during collection.",
                    script.display()
                )
            } else {
                format!(
                    "{} exited {}: the host was not quiet, so throughput families are marked \
                     contaminated. RSS, binary size and correctness are deterministic and stay \
                     comparable.",
                    script.display(),
                    o.status
                        .code()
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "on a signal".into())
                )
            };
            Guard { passed, load, note }
        }
    }
}

fn loadavg() -> Option<(f64, f64, f64)> {
    let text = fs::read_to_string("/proc/loadavg").ok()?;
    let mut it = text.split_whitespace();
    let mut next = || it.next().and_then(|v| v.parse::<f64>().ok());
    Some((next()?, next()?, next()?))
}

pub fn toolchain() -> String {
    match Command::new("rustc").arg("--version").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => format!("unknown: rustc --version exited {}", o.status),
        Err(e) => format!("unknown: rustc --version failed: {e}"),
    }
}

/// Anything that cannot be read is null, never a plausible-looking default.
pub fn host() -> Json {
    let cpu = proc_field("/proc/cpuinfo", "model name")
        .map(Json::Str)
        .unwrap_or(Json::Null);
    let cores = std::thread::available_parallelism()
        .map(|n| Json::Num(n.get() as f64))
        .unwrap_or(Json::Null);
    let ram_gib = proc_field("/proc/meminfo", "MemTotal")
        .and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
        .map(|kib| Json::Num((kib / 1_048_576.0).round()))
        .unwrap_or(Json::Null);
    let store = std::env::var("REGOLITH_BENCH_STORE").unwrap_or_else(|_| {
        "unrecorded (set REGOLITH_BENCH_STORE to name the filesystem the benches ran on)".into()
    });
    Json::obj(vec![
        ("cpu", cpu),
        ("cores", cores),
        ("ram_gib", ram_gib),
        ("store", Json::Str(store)),
    ])
}

fn proc_field(path: &str, field: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(field)
            && let Some(v) = rest.trim_start().strip_prefix(':')
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Plan constants, not measurements: the success criteria in
/// docs/plans/regolith-production.md, carried so a renderer can draw the target
/// side of a comparison when only one run has been collected.
const PLAN_TARGETS: &str = r#"{
  "note": "From docs/plans/regolith-production.md success criteria. NOT measurements.",
  "rss_point": {
    "Empty DB, 8 MiB cache budget": 12.0,
    "RSS at 4 GiB data, 192 MiB budget": 250.0,
    "WAL replay peak, 533 MiB log": 70.0,
    "Open floor, smallest config": 1.7,
    "1 GiB value put, peak": 3719.0
  },
  "correctness": { "Pessimistic txn": 100.0, "Optimistic txn": 100.0 },
  "viability": {
    "WASM (browser / wasmtime)": "works, under 4 MiB",
    "Embedded, 1-4 MiB budget": "reachable",
    "Serializable isolation": "available",
    "Elle-verified history": "passes at serializable"
  },
  "binary_size_kib": {
    "native": 359.4,
    "wasm32-wasip1": 349.6,
    "wasm32-wasip1+opt-Oz": 308.9,
    "policy": "budget: no regression; net of kovan +147.4 published by PR 3"
  }
}"#;

pub fn plan_targets() -> Json {
    json::parse(PLAN_TARGETS).unwrap_or_else(|e| die(&format!("PLAN_TARGETS is malformed: {e}")))
}

pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant's civil_from_days: days since 1970-01-01 to a civil date,
/// exact for every date the proleptic Gregorian calendar defines.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A run file is append-only once published, so an existing path is an error
/// rather than an overwrite.
pub fn write_new(path: &Path, body: &str) {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir).unwrap_or_else(|e| die(&format!("create {}: {e}", dir.display())));
    }
    let mut f = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => die(&format!(
            "{} already exists. A run file is append-only once published: re-measuring produces a \
             new file with a new timestamp, so a comparison can always be reproduced. Collect \
             under a different --label.",
            path.display()
        )),
        Err(e) => die(&format!("create {}: {e}", path.display())),
    };
    f.write_all(body.as_bytes())
        .unwrap_or_else(|e| die(&format!("write {}: {e}", path.display())));
}

pub fn die(why: &str) -> ! {
    eprintln!("collect: {why}");
    std::process::exit(1)
}
