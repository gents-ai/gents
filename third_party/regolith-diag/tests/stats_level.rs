//! What a `Statistics` records at each `StatsLevel` on every path the engine
//! times: a write, its log write and sync, a read, an iterator's seek and
//! step, a flush and a compaction. Without timers none of them records a
//! latency, and the tickers and the size histograms still count; with
//! timers the same work records every latency, so a zero is never a path
//! the work missed.

// Native-only: a temporary directory and the compaction worker.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use regolith::{Db, DurabilityMode, Histogram, Options, Statistics, StatsLevel, Ticker};

/// The latency histograms, one per timed path.
const LATENCIES: [Histogram; 7] = [
    Histogram::DbGet,
    Histogram::DbWrite,
    Histogram::DbIterSeek,
    Histogram::DbIterNext,
    Histogram::CompactionTime,
    Histogram::FlushTime,
    Histogram::WalWriteTime,
];

/// The size histograms.
const SIZES: [Histogram; 2] = [Histogram::BytesPerRead, Histogram::BytesPerWrite];

/// Writes synced one by one that fill enough memtables to flush, a
/// compaction, reads and a walk, measured by statistics at `level`.
fn measured(level: StatsLevel) -> Arc<Statistics> {
    let stats = Arc::new(Statistics::with_level(level));
    let dir = tempfile::tempdir().expect("tempdir");
    let options = Options::default()
        .write_buffer_size(4 * 1024)
        .durability(DurabilityMode::Immediate)
        .statistics(Some(Arc::clone(&stats)));
    let db = Db::open(dir.path(), options).expect("open");
    let payload = [7u8; 512];
    for i in 0..64u32 {
        regolith::through_stalls(|| db.put(&i.to_be_bytes(), &payload)).expect("put");
    }
    db.compact_range(None, None).wait().expect("compact");
    for i in 0..8u32 {
        assert!(db.get(&i.to_be_bytes()).expect("get").is_some(), "key {i}");
    }
    let mut iter = db.iter();
    iter.seek_to_first();
    while iter.valid() {
        iter.next();
    }
    drop(iter);
    db.close().expect("close");
    stats
}

/// Of `hists`, the ones holding at least one sample.
fn recorded<I>(stats: &Statistics, hists: I) -> Vec<Histogram>
where
    I: IntoIterator<Item = Histogram>,
{
    hists
        .into_iter()
        .filter(|&hist| stats.get_histogram_snapshot(hist).count > 0)
        .collect()
}

/// The tickers every level keeps, all moved by the work.
fn assert_counted(stats: &Statistics) {
    let still: Vec<Ticker> = [
        Ticker::KeysWritten,
        Ticker::KeysRead,
        Ticker::WalSyncCount,
        Ticker::FlushCount,
        Ticker::CompactionCount,
        Ticker::IterSeekCount,
        Ticker::IterNextCount,
    ]
    .into_iter()
    .filter(|&ticker| stats.get_ticker(ticker) == 0)
    .collect();
    assert!(still.is_empty(), "tickers that did not count: {still:?}");
}

#[test]
fn without_timers_no_path_records_a_latency_and_the_rest_still_counts() {
    let stats = measured(StatsLevel::ExceptTimers);
    assert_eq!(recorded(&stats, LATENCIES), [], "latencies recorded");
    assert_eq!(recorded(&stats, SIZES), SIZES, "sizes recorded");
    assert_counted(&stats);
}

#[test]
fn without_histograms_only_the_tickers_count() {
    let stats = measured(StatsLevel::ExceptHistogramOrTimers);
    assert_eq!(
        recorded(&stats, LATENCIES.into_iter().chain(SIZES)),
        [],
        "histograms recorded"
    );
    assert_counted(&stats);
}

#[test]
fn with_timers_every_path_records_its_latency() {
    let stats = measured(StatsLevel::All);
    assert_eq!(recorded(&stats, LATENCIES), LATENCIES, "latencies recorded");
    assert_eq!(recorded(&stats, SIZES), SIZES, "sizes recorded");
    assert_counted(&stats);
}
