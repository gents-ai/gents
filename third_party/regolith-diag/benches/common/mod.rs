//! Shared harness for the regolith benchmark suite.
//!
//! Every bench target links this module and reports through it, so sampling,
//! key shapes, process accounting, and the on-disk metric format stay in one
//! place. Process metrics are Linux-only and report 0 elsewhere; a bench that
//! prints them must say so rather than treating 0 as a measurement.

#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub mod counting_env;
pub mod start;

/// This process's arguments, without the `--bench` marker cargo appends.
///
/// `cargo bench --bench <name> -- <args>` passes `<args>` through and then
/// adds `--bench` of its own, which libtest reads and a `harness = false`
/// target has no reason to. Stripping it here is what lets a bench target
/// take real arguments: `collect` refuses an argument it does not know, and
/// `soak` treats any flag as a sign it was not meant to run at all, so
/// leaving the marker in place turns a real invocation into an error or a
/// silent skip.
pub fn args() -> Vec<String> {
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = argv.iter().rposition(|a| a == "--bench") {
        argv.remove(i);
    }
    argv
}

/// Deterministic splitmix64 generator: reproducible across runs and machines,
/// and well distributed from any seed including zero.
pub struct Rng(pub u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

pub fn rand_value(rng: &mut Rng, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        let take = std::cmp::min(8, len - out.len());
        out.extend_from_slice(&rng.next().to_le_bytes()[..take]);
    }
    out
}

/// Fixed-width so lexicographic key order matches numeric order.
pub fn key(i: u64) -> Vec<u8> {
    format!("key{i:012}").into_bytes()
}

#[cfg(target_os = "linux")]
fn status_kib(field: &str) -> u64 {
    let status = match fs::read_to_string("/proc/self/status") {
        Ok(s) => s,
        Err(_) => return 0,
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix(field) {
            return rest
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

#[cfg(target_os = "linux")]
pub fn rss_kib() -> u64 {
    status_kib("VmRSS:")
}

#[cfg(not(target_os = "linux"))]
pub fn rss_kib() -> u64 {
    0
}

#[cfg(target_os = "linux")]
pub fn peak_rss_kib() -> u64 {
    status_kib("VmHWM:")
}

#[cfg(not(target_os = "linux"))]
pub fn peak_rss_kib() -> u64 {
    0
}

#[cfg(target_os = "linux")]
pub fn cpu_seconds() -> f64 {
    // The /proc ABI reports CPU time in USER_HZ units, fixed at 100 on Linux
    // regardless of the kernel tick rate. comm may contain spaces and
    // parentheses, so fields are counted from the last ')': index 0 there is
    // field 3 (state), making utime field 14 -> index 11 and stime 15 -> 12.
    const USER_HZ: f64 = 100.0;
    let stat = match fs::read_to_string("/proc/self/stat") {
        Ok(s) => s,
        Err(_) => return 0.0,
    };
    let tail = match stat.rfind(')') {
        Some(i) => &stat[i + 1..],
        None => return 0.0,
    };
    let fields: Vec<&str> = tail.split_whitespace().collect();
    if fields.len() < 13 {
        return 0.0;
    }
    let utime: f64 = fields[11].parse().unwrap_or(0.0);
    let stime: f64 = fields[12].parse().unwrap_or(0.0);
    (utime + stime) / USER_HZ
}

#[cfg(not(target_os = "linux"))]
pub fn cpu_seconds() -> f64 {
    0.0
}

static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// A scratch directory removed when the guard drops. Rooted at TMPDIR when set
/// so a run can be pointed at a specific filesystem (tmpfs vs a real disk
/// changes storage-engine numbers a lot).
pub struct TempDb {
    pub dir: PathBuf,
}

impl TempDb {
    pub fn new(tag: &str) -> Self {
        let root = match std::env::var_os("TMPDIR") {
            Some(t) if !t.is_empty() => PathBuf::from(t),
            _ => std::env::temp_dir(),
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = root.join(format!(
            "regolith-bench-{tag}-{}-{nanos}-{seq}",
            std::process::id()
        ));
        fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("create bench dir {}: {e}", dir.display()));
        TempDb { dir }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        if let Err(e) = fs::remove_dir_all(&self.dir) {
            eprintln!("regolith-bench: leaked {}: {e}", self.dir.display());
        }
    }
}

/// Copy the database directory `from` to `to`, everything but the contents
/// of its `LOCK`, which the copy gets empty. The lock of a database still
/// open cannot be read on Windows, where its holder shares it for nothing
/// but deletion; an empty `LOCK` is one a read-write open adopts and stamps,
/// and one a read-only open, which never creates it, can lock shared.
pub fn copy_db(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap_or_else(|e| panic!("create {}: {e}", to.display()));
    for entry in fs::read_dir(from).unwrap_or_else(|e| panic!("read {}: {e}", from.display())) {
        let entry = entry.unwrap_or_else(|e| panic!("read {}: {e}", from.display()));
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        if src.is_dir() {
            copy_db(&src, &dst);
        } else if entry.file_name() == "LOCK" {
            fs::write(&dst, []).unwrap_or_else(|e| panic!("create {}: {e}", dst.display()));
        } else {
            fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
        }
    }
}

pub fn open(tag: &str, opts: regolith::Options) -> (TempDb, regolith::Db) {
    let tmp = TempDb::new(tag);
    let db = regolith::Db::open(tmp.path(), opts)
        .unwrap_or_else(|e| panic!("open db at {}: {e}", tmp.dir.display()));
    (tmp, db)
}

/// Whether `db`, which held no table and no sealed memtable when a timed
/// window began, sealed a memtable in it: one waits for its flush, or a table
/// was written. A bench whose rows must all time the write path alone checks
/// this, because a row that rotated timed a flush the others did not.
pub fn rotated(db: &regolith::Db) -> bool {
    let property = |name: &str| {
        db.get_int_property(name)
            .unwrap_or_else(|| panic!("the database does not report {name}"))
    };
    property("regolith.num-entries-imm-mem-tables") > 0
        || property("regolith.total-sst-files-size") > 0
}

pub fn default_opts() -> regolith::Options {
    regolith::Options::default()
}

/// Small enough to force flushes and compaction inside a bench run.
pub fn small_opts() -> regolith::Options {
    regolith::Options::default()
        .write_buffer_size(4 * 1024 * 1024)
        .block_cache_size(8 * 1024 * 1024)
        .block_cache_num_shard_bits(0)
}

#[allow(clippy::ptr_arg)]
pub fn median(v: &mut Vec<f64>) -> f64 {
    assert!(!v.is_empty(), "median of an empty sample");
    v.sort_by(|a, b| a.total_cmp(b));
    let mid = v.len() / 2;
    if v.len().is_multiple_of(2) {
        (v[mid - 1] + v[mid]) / 2.0
    } else {
        v[mid]
    }
}

pub fn min_max(v: &[f64]) -> (f64, f64) {
    assert!(!v.is_empty(), "min_max of an empty sample");
    let mut lo = v[0];
    let mut hi = v[0];
    for &x in &v[1..] {
        if x < lo {
            lo = x;
        }
        if x > hi {
            hi = x;
        }
    }
    (lo, hi)
}

/// Emit one metric family as JSON into the run file assembled by collect.rs.
///
/// With REGOLITH_BENCH_OUT set, one `{"family":..,"data":..}` record is appended
/// per call (JSON Lines). Without it, the family lands in `./bench-out/<name>.json`
/// so a single bench can be run standalone. `json` is written verbatim and must
/// already be valid JSON.
pub fn write_family(name: &str, json: &str) {
    let record = format!("{{\"family\":\"{name}\",\"data\":{json}}}\n");
    match std::env::var_os("REGOLITH_BENCH_OUT") {
        Some(p) if !p.is_empty() => {
            let path = PathBuf::from(p);
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .unwrap_or_else(|e| panic!("open {}: {e}", path.display()));
            f.write_all(record.as_bytes())
                .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        }
        _ => {
            let dir = PathBuf::from("bench-out");
            fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
            let path = dir.join(format!("{name}.json"));
            let mut f =
                File::create(&path).unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
            f.write_all(record.as_bytes())
                .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
        }
    }
}
