//! Write-ahead log replay time: how long `Db::open_read_only` takes to
//! replay one large log, healthy and with a damaged tail.
//!
//! The log is built once per process by single-threaded `Eventual` puts,
//! so every commit is its own group: the shape with the most framing per
//! byte. The writer is forgotten rather than closed, the state a crash
//! leaves, and its files are copied to a fresh directory the timed opens
//! read. A read-only open replays the log and writes nothing, so every
//! repetition replays the same bytes.
//!
//! Three shapes, each timed `REPS` times and reported as min, median and
//! max:
//! - `healthy`: the log as written;
//! - `zero_tail`: the log plus `TAIL_MIB` of zeros, as a filesystem that
//!   allocated blocks it never wrote leaves it;
//! - `garbage_tail`: the log plus `TAIL_MIB` of seeded garbage, the state
//!   a power cut leaves when a length reached the device before its data.
//!   A build that refuses this shape prints `refused` instead of a time.
//!
//! Environment: `REPLAY_MIB` (default 64), `TAIL_MIB` (default 16),
//! `REPS` (default 7).

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use regolith::{Db, DurabilityMode, Options};

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name}: expected an integer, got {v:?}"))
        })
        .unwrap_or(default)
}

fn opts(write_buffer_size: usize) -> Options {
    Options::default()
        .write_buffer_size(write_buffer_size)
        .durability(DurabilityMode::Eventual)
}

fn wal_files(db: &Path) -> Vec<PathBuf> {
    let mut wals: Vec<PathBuf> = fs::read_dir(db.join("wal"))
        .expect("wal dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    wals.sort();
    wals
}

/// Build a database whose newest log holds about `mib` MiB, and copy it
/// to `out`. Returns how many puts it holds.
fn build(mib: u64, scratch: &Path, out: &Path) -> u64 {
    let target = mib * 1024 * 1024;
    let src = scratch.join("src");
    let db = Db::open(&src, opts((target * 3) as usize)).expect("open");
    let value = [b'v'; 100];
    let mut n = 0u64;
    loop {
        for _ in 0..4096 {
            db.put(format!("key{n:012}").as_bytes(), &value)
                .expect("put");
            n += 1;
        }
        let len: u64 = wal_files(&src)
            .iter()
            .map(|p| fs::metadata(p).expect("stat").len())
            .sum();
        if len >= target {
            break;
        }
    }
    // The state a crash leaves: no close, so no flush and no final record.
    std::mem::forget(db);
    common::copy_db(&src, out);
    n
}

fn garbage(len: usize, mut seed: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        out.extend_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    out.truncate(len);
    out
}

fn time_opens(label: &str, db: &Path, reps: u64, wal_bytes: u64, expect_keys: Option<u64>) {
    let mut micros = Vec::new();
    for _ in 0..reps {
        let start = Instant::now();
        let opened = Db::open_read_only(db, opts(1 << 30));
        let elapsed = start.elapsed().as_micros() as u64;
        match opened {
            Ok(d) => {
                if let Some(n) = expect_keys {
                    let last = format!("key{:012}", n - 1);
                    assert!(
                        d.get(last.as_bytes()).expect("get").is_some(),
                        "{label}: the last put did not replay"
                    );
                }
                drop(d);
                micros.push(elapsed);
            }
            Err(e) => {
                println!("{label:<14} refused: {e}");
                return;
            }
        }
    }
    micros.sort_unstable();
    let median = micros[micros.len() / 2];
    let mib = wal_bytes as f64 / (1024.0 * 1024.0);
    println!(
        "{label:<14} log {mib:7.1} MiB  min {:8.1} ms  median {:8.1} ms  max {:8.1} ms  {:7.0} MiB/s",
        micros[0] as f64 / 1000.0,
        median as f64 / 1000.0,
        micros[micros.len() - 1] as f64 / 1000.0,
        mib / (median as f64 / 1e6),
    );
}

fn main() {
    let mib = env_u64("REPLAY_MIB", 64);
    let tail_mib = env_u64("TAIL_MIB", 16);
    let reps = env_u64("REPS", 7).max(1);

    let scratch = tempfile::TempDir::new().expect("tempdir");
    let healthy = scratch.path().join("healthy");
    let n = build(mib, scratch.path(), &healthy);
    let wal = wal_files(&healthy).pop().expect("one log");
    let wal_bytes = fs::metadata(&wal).expect("stat").len();
    println!("wal_replay: {n} puts of 100-byte values, log {wal_bytes} bytes");
    time_opens("healthy", &healthy, reps, wal_bytes, Some(n));

    let tail = (tail_mib * 1024 * 1024) as usize;
    for (label, bytes) in [
        ("zero_tail", vec![0u8; tail]),
        ("garbage_tail", garbage(tail, 0x5EED)),
    ] {
        let dir = scratch.path().join(label);
        common::copy_db(&healthy, &dir);
        let wal = wal_files(&dir).pop().expect("one log");
        let mut all = fs::read(&wal).expect("read");
        all.extend_from_slice(&bytes);
        fs::write(&wal, &all).expect("write");
        time_opens(label, &dir, reps, all.len() as u64, Some(n));
    }
}
