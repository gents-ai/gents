//! Memory-footprint bench: every number here is bytes, not time.
//!
//! Custom harness, not criterion. Each sample runs in a fresh child process (a
//! re-exec of this binary with `REGOLITH_MEM_CHILD` set) because a freed allocation
//! stays mapped in the process: measuring two configurations in one process
//! lets the first one's arena silently become the second one's floor.
//!
//! Sections: block-cache shard sweep at a pinned cache budget, resident set
//! after a bulk write at a fixed nominal budget, WAL replay peak against the
//! log it replays, the smallest option set that opens, and the floor of a
//! 1-4 MiB embedded profile.

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regolith::{Db, Options, WriteBatch};

const CHILD_ENV: &str = "REGOLITH_MEM_CHILD";
/// Pinned across the shard sweep so shard count is the only variable.
const PINNED_CACHE: usize = 8 * 1024 * 1024;
const FILL_WRITE_BUFFER: usize = 64 * 1024 * 1024;
const FILL_CACHE: usize = 64 * 1024 * 1024;
const EMBEDDED_WRITE_BUFFER: usize = 1024 * 1024;
const EMBEDDED_CACHE: usize = 1024 * 1024;
const METRICS: bool = cfg!(target_os = "linux");

type Fields = Vec<(String, String)>;

fn main() {
    if let Some(mode) = std::env::var_os(CHILD_ENV) {
        let mode = mode.to_string_lossy().into_owned();
        child(&mode);
        return;
    }
    parent(Cfg::parse(std::env::args().skip(1).collect()));
}

// ---------------------------------------------------------------- child side

fn base_fields() -> Fields {
    vec![
        ("rss_kib".to_string(), common::rss_kib().to_string()),
        ("peak_kib".to_string(), common::peak_rss_kib().to_string()),
        ("cpu_s".to_string(), format!("{:.3}", common::cpu_seconds())),
    ]
}

fn push(f: &mut Fields, k: &str, v: impl ToString) {
    f.push((k.to_string(), v.to_string()));
}

/// Results travel back as one whitespace-separated `k=v` line, so anything a
/// value could carry that would split that grid is folded down first.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_whitespace() || c == '"' || c == '\\' || c == '=' {
                '_'
            } else {
                c
            }
        })
        .take(160)
        .collect()
}

fn num(s: &str) -> u64 {
    s.parse()
        .unwrap_or_else(|_| panic!("expected a number, got {s:?}"))
}

fn split2<'a>(rest: &'a str, shape: &str) -> (&'a str, &'a str) {
    rest.split_once(':')
        .unwrap_or_else(|| panic!("child mode expected {shape}, got {rest:?}"))
}

fn child(mode: &str) {
    let (kind, rest) = mode.split_once(':').unwrap_or((mode, ""));
    let fields = match kind {
        "baseline" => base_fields(),
        "shards" => child_shards(num(rest) as u32),
        "fill" => {
            let (mib, bits) = split2(rest, "fill:<mib>:<shard_bits>");
            child_fill(num(mib), num(bits) as u32)
        }
        "floor" => child_floor(num(rest) as usize),
        "embedded" => child_embedded(num(rest)),
        "crash" => {
            let (mib, path) = split2(rest, "crash:<mib>:<path>");
            child_crash(num(mib), Path::new(path))
        }
        "replay" => {
            let (mib, path) = split2(rest, "replay:<mib>:<path>");
            child_replay(num(mib), Path::new(path))
        }
        other => panic!("unknown child mode {other:?}"),
    };
    let body: Vec<String> = fields.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("RESULT {}", body.join(" "));
}

fn fill(db: &Db, mib: u64, value_len: usize) {
    let mut rng = common::Rng::new(0x5EED_1234);
    let entries = (mib * 1024 * 1024) / value_len as u64;
    let mut i = 0u64;
    while i < entries {
        let mut batch = WriteBatch::new();
        let stop = std::cmp::min(i + 256, entries);
        while i < stop {
            batch.put(&common::key(i), &common::rand_value(&mut rng, value_len));
            i += 1;
        }
        db.write(batch).expect("write batch");
    }
}

fn child_shards(bits: u32) -> Fields {
    let opts = Options::default()
        .block_cache_size(PINNED_CACHE)
        .block_cache_num_shard_bits(bits);
    let (_tmp, db) = common::open("mem-shards", opts);
    assert!(
        db.get(b"absent").expect("get on empty db").is_none(),
        "empty db returned a value"
    );
    let mut f = base_fields();
    push(&mut f, "shard_bits", bits);
    push(&mut f, "cache_bytes", PINNED_CACHE);
    f
}

fn child_fill(mib: u64, bits: u32) -> Fields {
    let opts = Options::default()
        .write_buffer_size(FILL_WRITE_BUFFER)
        .max_write_buffer_number(2)
        .block_cache_size(FILL_CACHE)
        .block_cache_num_shard_bits(bits);
    let (_tmp, db) = common::open("mem-fill", opts);
    let started = Instant::now();
    fill(&db, mib, 1024);
    let elapsed = started.elapsed().as_secs_f64();
    let mut f = base_fields();
    push(&mut f, "written_mib", mib);
    push(&mut f, "shard_bits", bits);
    push(&mut f, "seconds", format!("{elapsed:.3}"));
    f
}

/// Every byte-sized knob is driven off one scale so the floor ladder has a
/// single independent variable.
fn floor_opts(scale: usize) -> Options {
    Options::default()
        .write_buffer_size(scale)
        .block_size(std::cmp::max(scale, 64))
        .block_cache_size(scale)
        .block_cache_num_shard_bits(0)
        .level_base_bytes(scale as u64)
        .target_file_size(scale as u64)
        .metadata_block_size(std::cmp::max(scale, 64))
}

fn child_floor(scale: usize) -> Fields {
    let tmp = common::TempDb::new("mem-floor");
    match Db::open(tmp.path(), floor_opts(scale)) {
        Ok(db) => {
            let usable = db.put(b"k", b"v").is_ok() && matches!(db.get(b"k"), Ok(Some(_)));
            let mut f = base_fields();
            push(&mut f, "scale_bytes", scale);
            push(&mut f, "opened", true);
            push(&mut f, "usable", usable);
            push(&mut f, "error", "null");
            f
        }
        Err(e) => {
            let mut f = base_fields();
            push(&mut f, "scale_bytes", scale);
            push(&mut f, "opened", false);
            push(&mut f, "usable", false);
            push(&mut f, "error", sanitize(&e.to_string()));
            f
        }
    }
}

fn child_embedded(mib: u64) -> Fields {
    let opts = Options::default()
        .write_buffer_size(EMBEDDED_WRITE_BUFFER)
        .max_write_buffer_number(2)
        .block_cache_size(EMBEDDED_CACHE)
        .block_cache_num_shard_bits(0)
        .block_size(4096)
        .level_base_bytes(4 * 1024 * 1024)
        .target_file_size(1024 * 1024);
    let (_tmp, db) = common::open("mem-embedded", opts);
    let empty_rss = common::rss_kib();
    fill(&db, mib, 256);
    let mut f = base_fields();
    push(&mut f, "empty_rss_kib", empty_rss);
    push(&mut f, "written_mib", mib);
    push(
        &mut f,
        "nominal_kib",
        (2 * EMBEDDED_WRITE_BUFFER + EMBEDDED_CACHE) / 1024,
    );
    f
}

/// A write buffer larger than the payload keeps every record in the one WAL, so
/// the reopen below has to replay the whole log instead of reading an SSTable.
fn crash_opts(mib: u64) -> Options {
    Options::default()
        .write_buffer_size((mib as usize + 64) * 1024 * 1024)
        .max_write_buffer_number(2)
        .block_cache_size(1024 * 1024)
        .block_cache_num_shard_bits(0)
}

fn child_crash(mib: u64, path: &Path) -> ! {
    let db = Db::open(path, crash_opts(mib)).expect("open db for crash");
    fill(&db, mib, 1024);
    // No close, no flush, no destructors: the log keeps whatever reached the
    // file, exactly as it would after a kill -9.
    std::process::exit(9);
}

fn child_replay(mib: u64, path: &Path) -> Fields {
    let started = Instant::now();
    let db = Db::open(path, crash_opts(mib)).expect("reopen after crash");
    let open_s = started.elapsed().as_secs_f64();
    let mut f = base_fields();
    push(&mut f, "open_s", format!("{open_s:.3}"));
    push(
        &mut f,
        "first_key_present",
        matches!(db.get(&common::key(0)), Ok(Some(_))),
    );
    f
}

// --------------------------------------------------------------- parent side

struct Cfg {
    write_mib: Vec<u64>,
    max_shard_bits: u32,
    wal_mib: u64,
    embedded_mib: u64,
    embedded_only: bool,
}

fn usage() -> &'static str {
    "usage: memory [--profile full|embedded] [--write-mib 1024,2048,4096]\n\
     \x20             [--max-shard-bits 8] [--wal-mib 256] [--embedded-mib 64] [--test]\n\
     \x20--test shrinks every size for a smoke run and overrides earlier size flags."
}

impl Cfg {
    fn parse(args: Vec<String>) -> Cfg {
        let mut c = Cfg {
            write_mib: vec![1024, 2048, 4096],
            max_shard_bits: regolith::MAX_BLOCK_CACHE_SHARD_BITS,
            wal_mib: 256,
            embedded_mib: 64,
            embedded_only: false,
        };
        let mut it = args.into_iter();
        while let Some(a) = it.next() {
            let mut value = || {
                it.next()
                    .unwrap_or_else(|| panic!("{a} needs a value\n{}", usage()))
            };
            match a.as_str() {
                "--bench" => {}
                "--test" => {
                    c.write_mib = vec![16];
                    c.max_shard_bits = 4;
                    c.wal_mib = 8;
                    c.embedded_mib = 4;
                }
                "--profile" => match value().as_str() {
                    "embedded" => c.embedded_only = true,
                    "full" => c.embedded_only = false,
                    o => panic!("unknown profile {o:?}, expected full or embedded"),
                },
                "--write-mib" => {
                    c.write_mib = value().split(',').map(|s| num(s.trim())).collect();
                    assert!(!c.write_mib.is_empty(), "--write-mib needs one size");
                }
                "--max-shard-bits" => {
                    c.max_shard_bits = num(&value()) as u32;
                    assert!(
                        c.max_shard_bits <= regolith::MAX_BLOCK_CACHE_SHARD_BITS,
                        "--max-shard-bits must be <= {}",
                        regolith::MAX_BLOCK_CACHE_SHARD_BITS
                    );
                }
                "--wal-mib" => c.wal_mib = num(&value()),
                "--embedded-mib" => c.embedded_mib = num(&value()),
                "-h" | "--help" => {
                    println!("{}", usage());
                    std::process::exit(0);
                }
                o => {
                    eprintln!("memory: unknown argument {o:?}\n{}", usage());
                    std::process::exit(2);
                }
            }
        }
        c
    }
}

fn run_raw(mode: &str, timeout_s: u64) -> std::io::Result<(Option<i32>, String)> {
    let exe = std::env::current_exe()?;
    let mut ch = Command::new(exe)
        .env(CHILD_ENV, mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let mut backoff = Duration::from_millis(1);
    let status = loop {
        if let Some(s) = ch.try_wait()? {
            break Some(s);
        }
        if Instant::now() >= deadline {
            let _ = ch.kill();
            let _ = ch.wait();
            break None;
        }
        std::thread::sleep(backoff);
        backoff = std::cmp::min(backoff * 2, Duration::from_millis(50));
    };
    let mut out = String::new();
    if let Some(mut pipe) = ch.stdout.take() {
        use std::io::Read;
        pipe.read_to_string(&mut out)?;
    }
    match status {
        Some(s) => Ok((s.code(), out)),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("child {mode} exceeded {timeout_s}s"),
        )),
    }
}

fn run(mode: &str, timeout_s: u64) -> Fields {
    let (code, out) = run_raw(mode, timeout_s).unwrap_or_else(|e| panic!("child {mode}: {e}"));
    assert_eq!(code, Some(0), "child {mode} exited with {code:?}");
    let line = out
        .lines()
        .find(|l| l.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("child {mode} printed no RESULT line"));
    line["RESULT ".len()..]
        .split_whitespace()
        .filter_map(|kv| {
            kv.split_once('=')
                .map(|(k, v)| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn get(f: &Fields, k: &str) -> String {
    f.iter()
        .find(|(a, _)| a == k)
        .map(|(_, b)| b.clone())
        .unwrap_or_else(|| panic!("child result has no field {k}"))
}

fn getu(f: &Fields, k: &str) -> u64 {
    num(&get(f, k))
}

/// The printed summary and the JSON record are built from one field list, so a
/// human reading the log and a tool reading the file cannot be told different
/// numbers. Returns the JSON object for the row it just printed.
fn emit(section: &str, kv: &[(&str, String)]) -> String {
    let line: Vec<String> = kv.iter().map(|(k, v)| format!("{k}={v}")).collect();
    println!("memory.{section} {}", line.join(" "));
    let obj: Vec<String> = kv
        .iter()
        .map(|(k, v)| format!("\"{k}\":{}", json_value(v)))
        .collect();
    format!("{{{}}}", obj.join(","))
}

/// Numbers, booleans and null go through bare; everything else becomes a JSON
/// string. The digit filter keeps `inf` and `NaN` out, which JSON cannot spell.
fn json_value(v: &str) -> String {
    if v == "true" || v == "false" || v == "null" {
        return v.to_string();
    }
    let numeric = !v.is_empty()
        && v.bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
        && v.parse::<f64>().is_ok();
    if numeric {
        v.to_string()
    } else {
        format!("\"{v}\"")
    }
}

fn wal_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir.join("wal")) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("log") {
                total += e.metadata().map_or(0, |m| m.len());
            }
        }
    }
    total
}

fn parent(cfg: Cfg) {
    println!("regolith memory bench (bytes, not time)");
    println!(
        "memory.meta process_metrics={METRICS} source={}",
        if METRICS {
            "/proc/self/status+stat"
        } else {
            "not-available-on-this-platform"
        }
    );

    let baseline = run("baseline", 120);
    let base_rss = getu(&baseline, "rss_kib");
    emit(
        "baseline",
        &[
            ("rss_kib", base_rss.to_string()),
            ("peak_kib", get(&baseline, "peak_kib")),
        ],
    );

    let mut sections = vec!["baseline"];
    let mut shard_json = "null".to_string();
    let mut write_json = "null".to_string();
    let mut wal_json = "null".to_string();
    let mut floor_json = "null".to_string();

    if !cfg.embedded_only {
        sections.push("shard_sweep");
        let mut rows = Vec::new();
        for bits in 0..=cfg.max_shard_bits {
            let f = run(&format!("shards:{bits}"), 600);
            let rss = getu(&f, "rss_kib");
            // 2^bits is what the option asks for, not necessarily what the
            // cache builds: it halves the count while a shard would fall under
            // its minimum capacity, and the built count is not public. Reported
            // as requested so the column never claims a shard that was clamped
            // away; a sweep that flattens at the top has hit that clamp.
            rows.push(emit(
                "shard_sweep",
                &[
                    ("shard_bits", bits.to_string()),
                    ("requested_shards", (1u64 << bits).to_string()),
                    ("cache_bytes", PINNED_CACHE.to_string()),
                    ("rss_kib", rss.to_string()),
                    ("peak_kib", get(&f, "peak_kib")),
                    (
                        "over_baseline_kib",
                        rss.saturating_sub(base_rss).to_string(),
                    ),
                ],
            ));
        }
        shard_json = format!("[{}]", rows.join(","));

        sections.push("write_sweep");
        let default_bits = Options::default().get_block_cache_num_shard_bits();
        let variants: Vec<u32> = if default_bits == 0 {
            vec![0]
        } else {
            vec![default_bits, 0]
        };
        let nominal = (2 * FILL_WRITE_BUFFER + FILL_CACHE) / (1024 * 1024);
        let mut rows = Vec::new();
        for &mib in &cfg.write_mib {
            for &bits in &variants {
                let f = run(&format!("fill:{mib}:{bits}"), 600 + mib * 4);
                rows.push(emit(
                    "write_sweep",
                    &[
                        ("written_mib", mib.to_string()),
                        ("shard_bits", bits.to_string()),
                        ("nominal_mib", nominal.to_string()),
                        ("rss_kib", get(&f, "rss_kib")),
                        ("peak_kib", get(&f, "peak_kib")),
                        ("seconds", get(&f, "seconds")),
                        ("cpu_s", get(&f, "cpu_s")),
                    ],
                ));
            }
        }
        write_json = format!("[{}]", rows.join(","));

        sections.push("wal_replay");
        let tmp = common::TempDb::new("mem-crash");
        let path = tmp.path().display().to_string();
        let mib = cfg.wal_mib;
        let (code, _) = run_raw(&format!("crash:{mib}:{path}"), 600 + mib * 4)
            .unwrap_or_else(|e| panic!("crash child: {e}"));
        let log_bytes = wal_bytes(tmp.path());
        let f = run(&format!("replay:{mib}:{path}"), 600 + mib * 4);
        let peak = getu(&f, "peak_kib");
        let ratio = if log_bytes > 0 {
            format!("{:.2}", (peak as f64 * 1024.0) / log_bytes as f64)
        } else {
            "null".to_string()
        };
        wal_json = emit(
            "wal_replay",
            &[
                ("written_mib", mib.to_string()),
                ("crashed_with_9", (code == Some(9)).to_string()),
                ("log_bytes", log_bytes.to_string()),
                ("replay_rss_kib", get(&f, "rss_kib")),
                ("replay_peak_kib", peak.to_string()),
                ("open_s", get(&f, "open_s")),
                ("first_key_present", get(&f, "first_key_present")),
                ("peak_over_log", ratio),
            ],
        );

        sections.push("open_floor");
        let mut rows = Vec::new();
        for scale in [1usize, 16, 256, 4096, 65536, 1024 * 1024] {
            let f = run(&format!("floor:{scale}"), 300);
            rows.push(emit(
                "open_floor",
                &[
                    ("scale_bytes", scale.to_string()),
                    ("opened", get(&f, "opened")),
                    ("usable", get(&f, "usable")),
                    ("rss_kib", get(&f, "rss_kib")),
                    ("error", get(&f, "error")),
                ],
            ));
        }
        floor_json = format!("[{}]", rows.join(","));
    }

    sections.push("embedded");
    let f = run(&format!("embedded:{}", cfg.embedded_mib), 900);
    let embedded_json = emit(
        "embedded",
        &[
            ("nominal_kib", get(&f, "nominal_kib")),
            ("empty_rss_kib", get(&f, "empty_rss_kib")),
            ("written_mib", cfg.embedded_mib.to_string()),
            ("rss_kib", get(&f, "rss_kib")),
            ("peak_kib", get(&f, "peak_kib")),
        ],
    );

    let sections_json: Vec<String> = sections.iter().map(|s| format!("\"{s}\"")).collect();
    let data = format!(
        "{{\"process_metrics_available\":{METRICS},\"sections_run\":[{}],\
         \"baseline_rss_kib\":{base_rss},\"pinned_cache_bytes\":{PINNED_CACHE},\
         \"shard_sweep\":{shard_json},\"write_sweep\":{write_json},\
         \"wal_replay\":{wal_json},\"open_floor\":{floor_json},\
         \"embedded\":{embedded_json}}}",
        sections_json.join(",")
    );
    common::write_family("memory", &data);
}
