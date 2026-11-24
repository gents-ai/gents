//! What an external-file ingest costs, and what it costs every other writer.
//!
//! A writer thread commits one put at a time for the whole run and times
//! each commit, while the main thread ingests files built with
//! `SstFileWriter`, one call per file. The writer's keys never fall inside
//! an ingested file's range, so no memtable has to be flushed for the
//! ingest, and the write buffer holds the whole run, so the writer never
//! rotates: a rotation flushes its memtable with the commit pipeline held
//! and would stall the writer by itself, whatever the ingest does.
//! Reported:
//!
//! - ingest time: wall time of one `ingest_external_files` call, median,
//!   min and max over the files;
//! - commit latency while an ingest is in flight (a commit whose span
//!   overlaps an ingest call), p50, p99 and max, beside the same numbers
//!   for the commits that overlapped no ingest;
//! - point reads of a freshly ingested table through a block cache too small
//!   to hold it, so nearly every read decodes a block, p50 and p99: what a
//!   read pays until a compaction rewrites the table.
//!
//! `--quick` (or `--test`) ingests fewer, smaller files.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use regolith::{IngestOptions, SstFileWriter};

const VALUE_BYTES: usize = 100;
const GAP: Duration = Duration::from_millis(50);

/// One timed commit: when it started and how long it took, in nanoseconds
/// from the run's origin.
#[derive(Clone, Copy)]
struct Commit {
    start: u64,
    took: u64,
}

fn percentile(sorted: &[u64], p: f64) -> f64 {
    assert!(!sorted.is_empty(), "percentile of an empty sample");
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx] as f64 / 1_000.0
}

fn build_file(path: &std::path::Path, file: usize, entries: usize) {
    let mut writer = SstFileWriter::create(path, &common::default_opts())
        .unwrap_or_else(|e| panic!("create {}: {e}", path.display()));
    let mut rng = common::Rng::new(0x1A6E_5700 ^ file as u64);
    for i in 0..entries {
        let value = common::rand_value(&mut rng, VALUE_BYTES);
        writer
            .put(format!("ing/{file:04}/{i:010}").as_bytes(), &value)
            .unwrap_or_else(|e| panic!("put: {e}"));
    }
    writer
        .finish()
        .unwrap_or_else(|e| panic!("finish {}: {e}", path.display()));
}

fn summary(label: &str, mut ns: Vec<u64>) -> String {
    if ns.is_empty() {
        println!("  {label:<26} no commits");
        return format!("{{\"commits\":0,\"label\":\"{label}\"}}");
    }
    ns.sort_unstable();
    let (p50, p99, max) = (
        percentile(&ns, 0.50),
        percentile(&ns, 0.99),
        *ns.last().unwrap_or(&0) as f64 / 1_000.0,
    );
    println!(
        "  {label:<26} {:>8} commits  p50 {p50:>9.1} us  p99 {p99:>10.1} us  max {max:>11.1} us",
        ns.len()
    );
    format!(
        "{{\"label\":\"{label}\",\"commits\":{},\"p50_us\":{p50:.1},\"p99_us\":{p99:.1},\"max_us\":{max:.1}}}",
        ns.len()
    )
}

fn main() {
    let quick = common::args()
        .iter()
        .any(|a| a == "--quick" || a == "--test");
    let (files, entries) = if quick { (3, 20_000) } else { (8, 200_000) };

    let staging = common::TempDb::new("ingest-staging");
    let paths: Vec<_> = (0..files)
        .map(|file| {
            let path = staging.path().join(format!("ingest-{file}.sst"));
            build_file(&path, file, entries);
            path
        })
        .collect();
    let file_bytes = std::fs::metadata(&paths[0]).map(|m| m.len()).unwrap_or(0);

    let (tmp, db) = common::open("ingest", common::default_opts().write_buffer_size(1 << 30));
    let db = Arc::new(db);
    let origin = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let db = Arc::clone(&db);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut rng = common::Rng::new(0xC0_441);
            let value = common::rand_value(&mut rng, VALUE_BYTES);
            let mut commits = Vec::with_capacity(1 << 20);
            let mut i = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let started = Instant::now();
                db.put(&common::key(i), &value)
                    .unwrap_or_else(|e| panic!("put: {e}"));
                commits.push(Commit {
                    start: started.duration_since(origin).as_nanos() as u64,
                    took: started.elapsed().as_nanos() as u64,
                });
                i += 1;
            }
            commits
        })
    };

    std::thread::sleep(GAP);
    let mut windows = Vec::with_capacity(files);
    let mut ingest_ms = Vec::with_capacity(files);
    for path in &paths {
        let started = Instant::now();
        db.ingest_external_files(
            std::slice::from_ref(path),
            IngestOptions {
                snapshot_consistency: false,
                ..IngestOptions::default()
            },
        )
        .wait()
        .unwrap_or_else(|e| panic!("ingest {}: {e}", path.display()));
        let took = started.elapsed();
        let from = started.duration_since(origin).as_nanos() as u64;
        windows.push((from, from + took.as_nanos() as u64));
        ingest_ms.push(took.as_secs_f64() * 1e3);
        std::thread::sleep(GAP);
    }
    stop.store(true, Ordering::Relaxed);
    let commits = writer.join().unwrap_or_else(|_| panic!("writer panicked"));

    let overlaps = |c: &Commit| {
        windows
            .iter()
            .any(|&(from, to)| c.start < to && c.start + c.took > from)
    };
    let (during, idle): (Vec<Commit>, Vec<Commit>) = commits.iter().partition(|c| overlaps(c));

    let (lo, hi) = common::min_max(&ingest_ms);
    let median = common::median(&mut ingest_ms);
    println!(
        "ingest: {files} files of {entries} entries ({} KiB each), copied (move_files = false)",
        file_bytes / 1024
    );
    println!(
        "  ingest time                median {median:>9.2} ms  min {lo:>9.2} ms  max {hi:>9.2} ms"
    );
    let during_json = summary(
        "commits during an ingest",
        during.iter().map(|c| c.took).collect(),
    );
    let idle_json = summary(
        "commits with no ingest",
        idle.iter().map(|c| c.took).collect(),
    );

    let db = Arc::try_unwrap(db).unwrap_or_else(|_| panic!("the writer still holds the database"));
    db.close().unwrap_or_else(|e| panic!("close: {e}"));
    drop(tmp);

    let reads_json = cold_reads(&paths[0], 0, entries);
    common::write_family(
        "ingest",
        &format!(
            "{{\"files\":{files},\"entries\":{entries},\"file_bytes\":{file_bytes},\
             \"ingest_ms\":{{\"median\":{median:.2},\"min\":{lo:.2},\"max\":{hi:.2}}},\
             \"commit_latency\":[{during_json},{idle_json}],\"cold_reads\":{reads_json}}}"
        ),
    );
    drop(staging);
}

/// Point reads of `path`, file number `file`, freshly ingested into a
/// database whose block cache holds a few blocks.
fn cold_reads(path: &std::path::Path, file: usize, entries: usize) -> String {
    let (tmp, db) = common::open(
        "ingest-reads",
        common::default_opts()
            .block_cache_size(64 * 1024)
            .block_cache_num_shard_bits(0),
    );
    db.ingest_external_files(
        std::slice::from_ref(&path.to_path_buf()),
        IngestOptions::default(),
    )
    .wait()
    .unwrap_or_else(|e| panic!("ingest {}: {e}", path.display()));
    let mut rng = common::Rng::new(0x5EAD);
    let reads = entries.min(50_000);
    let mut ns = Vec::with_capacity(reads);
    for _ in 0..reads {
        let i = rng.next() as usize % entries;
        let key = format!("ing/{file:04}/{i:010}");
        let started = Instant::now();
        let got = db
            .get(key.as_bytes())
            .unwrap_or_else(|e| panic!("get: {e}"));
        ns.push(started.elapsed().as_nanos() as u64);
        assert!(got.is_some(), "an ingested key is missing");
    }
    db.close().unwrap_or_else(|e| panic!("close: {e}"));
    drop(tmp);
    summary("reads of an ingested table", ns)
}
