//! RSS soak: resident memory over a sustained mixed workload.
//!
//! A custom harness rather than criterion: the quantity under test is retained
//! memory over time, and criterion measures time. One `soak` family record is
//! emitted per run, so several configurations of the same code can be soaked
//! into one run file and told apart by the `options` each entry carries.
//!
//! Usage:
//!   cargo bench --bench soak -- <seconds> <write_buffer_mib> <cache_mib> <shard_bits> <tag>
//! Defaults: 360 64 64 6 default

mod common;

use std::hint::black_box;
use std::time::Instant;

use regolith::{Options, Snapshot, through_stalls};

const KEYSPACE: u64 = 400_000;
const VALUE_BYTES: usize = 4096;
const SCAN_ENTRIES: usize = 32;
const SAMPLE_EVERY_OPS: u64 = 2_000;
const LIVE_SNAPSHOTS: usize = 8;
const MAX_SHARD_BITS: u32 = 16;
const SEED: u64 = 0x50AC_5EED;

/// Cumulative op mix out of 100: 55 writes, 20 reads, 15 scans, 8 deletes,
/// 2 snapshot hold-and-release.
const P_WRITE: u64 = 55;
const P_READ: u64 = 75;
const P_SCAN: u64 = 90;
const P_DELETE: u64 = 98;

const WORKLOAD: &str = "RSS over a sustained mixed workload: 55% writes/overwrites, \
20% point reads, 15% short range scans of 32 entries, 8% deletes, 2% snapshot \
hold-and-release over a 400k keyspace with 4 KiB values.";

/// Process RSS comes from /proc; elsewhere it is not a measurement at all.
const RSS_AVAILABLE: bool = cfg!(target_os = "linux");

fn main() {
    let argv = common::args();
    if argv.iter().any(|a| a.starts_with('-')) {
        eprintln!(
            "soak: skipped. It is a custom long-running harness, not a criterion bench, so `cargo bench` does not drive it."
        );
        eprintln!(
            "  cargo bench --bench soak -- <seconds> <write_buffer_mib> <cache_mib> <shard_bits> <tag>"
        );
        return;
    }

    let seconds: f64 = positional(&argv, 0, "seconds", 360.0);
    let wb_mib: u64 = positional(&argv, 1, "write_buffer_mib", 64);
    let cache_mib: u64 = positional(&argv, 2, "cache_mib", 64);
    let shard_bits: u32 = positional(&argv, 3, "shard_bits", 6);
    let tag = argv
        .get(4)
        .cloned()
        .unwrap_or_else(|| "default".to_string());

    if seconds <= 0.0 {
        die("seconds must be greater than zero");
    }
    if wb_mib == 0 || cache_mib == 0 {
        die("write_buffer_mib and cache_mib must be greater than zero");
    }
    if shard_bits > MAX_SHARD_BITS {
        die(&format!(
            "shard_bits {shard_bits} is above the {MAX_SHARD_BITS} ceiling this harness accepts"
        ));
    }
    if tag.is_empty()
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    {
        die("tag must be non-empty and only [A-Za-z0-9._-], it names a JSON field");
    }

    let defaults = Options::default();
    let buffers = defaults.get_max_write_buffer_number();
    let block_kib = defaults.get_block_size() / 1024;
    let opts = defaults
        .write_buffer_size((wb_mib * 1024 * 1024) as usize)
        .block_cache_size((cache_mib * 1024 * 1024) as usize)
        .block_cache_num_shard_bits(shard_bits);

    let (tmp, db) = common::open(&format!("soak-{tag}"), opts);
    let shards = 1u64 << shard_bits;
    let budget_mib = wb_mib * buffers as u64 + cache_mib;
    println!(
        "soak {tag}: {seconds}s, write_buffer {wb_mib} MiB x{buffers}, cache {cache_mib} MiB \
         across {shards} shards, {budget_mib} MiB nominal budget"
    );
    println!("  db dir: {}", tmp.path().display());
    if !RSS_AVAILABLE {
        eprintln!("  RSS is not available on this platform: the rss fields will be null, not zero");
    }

    let mut rng = common::Rng::new(SEED);
    // One value buffer, mutated per write: a fresh 4 KiB allocation per op
    // would put allocator behaviour into a measurement of engine retention.
    let mut value = common::rand_value(&mut rng, VALUE_BYTES);
    let mut live: Vec<Snapshot> = Vec::with_capacity(LIVE_SNAPSHOTS);

    let mut ops: u64 = 0;
    let mut writes: u64 = 0;
    let mut samples: Vec<(f64, f64, u64)> = Vec::new();
    let mut rss_peak = 0.0f64;
    let start = Instant::now();

    loop {
        if ops.is_multiple_of(SAMPLE_EVERY_OPS) {
            let elapsed = start.elapsed().as_secs_f64();
            let rss_mib = common::rss_kib() as f64 / 1024.0;
            samples.push((elapsed, rss_mib, ops));
            if rss_mib > rss_peak {
                rss_peak = rss_mib;
            }
            if elapsed >= seconds {
                break;
            }
        }

        let k = common::key(rng.next() % KEYSPACE);
        match rng.next() % 100 {
            r if r < P_WRITE => {
                value[..8].copy_from_slice(&rng.next().to_le_bytes());
                through_stalls(|| db.put(&k, &value))
                    .unwrap_or_else(|e| panic!("put at op {ops}: {e}"));
                writes += 1;
            }
            r if r < P_READ => {
                let v = db
                    .get(&k)
                    .unwrap_or_else(|e| panic!("get at op {ops}: {e}"));
                black_box(v);
            }
            r if r < P_SCAN => {
                let mut it = db.iter();
                it.seek(&k);
                let mut seen = 0usize;
                let mut bytes = 0usize;
                while it.valid() && seen < SCAN_ENTRIES {
                    bytes += it.key().map_or(0, |b| b.len()) + it.value().map_or(0, |b| b.len());
                    it.next();
                    seen += 1;
                }
                black_box(bytes);
            }
            r if r < P_DELETE => {
                through_stalls(|| db.delete(&k))
                    .unwrap_or_else(|e| panic!("delete at op {ops}: {e}"));
            }
            _ => {
                let snap = db.snapshot();
                let v = snap
                    .get(&k)
                    .unwrap_or_else(|e| panic!("snapshot get at op {ops}: {e}"));
                black_box(v);
                live.push(snap);
                if live.len() > LIVE_SNAPSHOTS {
                    live.remove(0);
                }
            }
        }
        ops += 1;
    }

    let elapsed = start.elapsed().as_secs_f64();
    let rss_end = common::rss_kib() as f64 / 1024.0;
    if rss_end > rss_peak {
        rss_peak = rss_end;
    }
    let hwm = common::peak_rss_kib() as f64 / 1024.0;
    if hwm > rss_peak {
        rss_peak = hwm;
    }
    let gib_written = writes as f64 * VALUE_BYTES as f64 / (1024.0 * 1024.0 * 1024.0);

    let label =
        format!("{tag} ({shards} cache shards, wb {wb_mib} MiB x{buffers}, cache {cache_mib} MiB)");
    let options = format!(
        r#"{{"write_buffer_mib":{wb_mib},"buffers":{buffers},"cache_mib":{cache_mib},"block_cache_num_shard_bits":{shard_bits},"block_kib":{block_kib}}}"#
    );
    let fields = vec![
        format!(r#""variant":"{tag}""#),
        format!(r#""label":"{label}""#),
        format!(r#""workload":"{WORKLOAD}""#),
        format!(r#""options":{options}"#),
        format!(r#""nominal_budget_mib":{budget_mib}"#),
        format!(r#""duration_s":{seconds}"#),
        format!(r#""elapsed_s":{elapsed:.3}"#),
        format!(r#""ops":{ops}"#),
        format!(r#""writes":{writes}"#),
        format!(r#""gib_written":{gib_written:.3}"#),
        format!(r#""rss_end_mib":{}"#, mib(rss_end)),
        format!(r#""rss_peak_mib":{}"#, mib(rss_peak)),
        format!(r#""rss_source":"{}""#, rss_source()),
        format!(r#""samples_t_rss_ops":{}"#, samples_json(&samples)),
    ];
    common::write_family("soak", &format!("{{{}}}", fields.join(",")));

    println!(
        "  {ops} ops in {elapsed:.1}s ({writes} writes, {gib_written:.3} GiB of values written)"
    );
    if RSS_AVAILABLE {
        println!(
            "  RSS end {rss_end:.1} MiB, peak {rss_peak:.1} MiB, {} samples",
            samples.len()
        );
    } else {
        println!(
            "  RSS not available on this platform, {} samples carry a null rss field",
            samples.len()
        );
    }
    println!("  family 'soak' written (REGOLITH_BENCH_OUT, else bench-out/soak.json)");
}

fn samples_json(samples: &[(f64, f64, u64)]) -> String {
    let mut out = String::with_capacity(samples.len() * 24 + 2);
    out.push('[');
    for (i, (t, rss, ops)) in samples.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&format!("[{t:.3},{},{ops}]", mib(*rss)));
    }
    out.push(']');
    out
}

/// Never renders an unmeasured RSS as 0: on a platform without /proc it is null.
fn mib(v: f64) -> String {
    if RSS_AVAILABLE {
        format!("{v:.1}")
    } else {
        "null".to_string()
    }
}

fn rss_source() -> &'static str {
    if RSS_AVAILABLE {
        "/proc/self/status VmRSS"
    } else {
        "not available on this platform: rss fields are null, not zero"
    }
}

fn positional<T: std::str::FromStr>(argv: &[String], i: usize, name: &str, default: T) -> T {
    match argv.get(i) {
        None => default,
        Some(s) => s.parse().unwrap_or_else(|_| {
            eprintln!("soak: {name} must be a number, got {s:?}");
            std::process::exit(2)
        }),
    }
}

fn die(why: &str) -> ! {
    eprintln!("soak: {why}");
    std::process::exit(2)
}
