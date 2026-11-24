//! Open and recovery time with many tables and many logs (E29, E30).
//!
//! A database is built once per process: `TABLES` flushes, one table each,
//! then a final memtable's worth of puts in the active log, and the writer
//! is forgotten rather than closed, the state a crash leaves. Each shape is
//! a copy of it, and each repetition opens a fresh copy read-write, so the
//! open does its whole recovery every time: it replays the manifest, judges
//! its end, replays the log, rewrites it into a new one, and removes the
//! logs it retires. Copying sits outside the timed window.
//!
//! Shapes, each timed `REPS` times and reported as min, median and max:
//! - `clean`: the database as the crash left it;
//! - `leftover_logs`: plus `LOGS` empty logs below every live one, as a
//!   flush whose log removal failed leaves them;
//! - `torn_manifest`: plus `TAIL_KIB` of seeded garbage after the
//!   manifest's last batch, as a power cut leaves an unsynced tail. A
//!   build that refuses this shape prints `refused` instead of a time.
//!
//! Environment: `TABLES` (default 256), `LOGS` (default 64), `TAIL_KIB`
//! (default 64), `REPS` (default 7).

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

fn opts() -> Options {
    Options::default()
        .write_buffer_size(1 << 30)
        .durability(DurabilityMode::Eventual)
        .max_background_compactions(0)
        .l0_compaction_trigger(1 << 20)
        .level0_slowdown_writes_trigger(0)
        .level0_stop_writes_trigger(0)
}

fn logs(db: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = fs::read_dir(db.join("wal"))
        .expect("wal dir")
        .flatten()
        .map(|e| e.path())
        .collect();
    logs.sort();
    logs
}

/// `tables` flushes of `per_table` puts each, then `per_table` more puts in
/// the active log, then a crash. Returns how many puts it holds.
fn build(dir: &Path, tables: u64, per_table: u64) -> u64 {
    let db = Db::open(dir, opts()).expect("open");
    let value = [b'v'; 100];
    let mut n = 0u64;
    for t in 0..=tables {
        for _ in 0..per_table {
            db.put(format!("key{n:012}").as_bytes(), &value)
                .expect("put");
            n += 1;
        }
        if t < tables {
            db.flush().expect("flush");
        }
    }
    std::mem::forget(db);
    n
}

/// An empty log with the header this build writes: a fresh database's
/// active log after a clean close.
fn empty_log(scratch: &Path) -> Vec<u8> {
    let dir = scratch.join("empty");
    let db = Db::open(&dir, opts()).expect("open");
    db.close().expect("close");
    drop(db);
    fs::read(logs(&dir).pop().expect("one log")).expect("read")
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

fn time_opens(label: &str, shape: &Path, scratch: &Path, reps: u64, puts: u64) {
    let mut micros = Vec::new();
    for rep in 0..reps {
        let dir = scratch.join(format!("{label}-{rep}"));
        common::copy_db(shape, &dir);
        let start = Instant::now();
        let opened = Db::open(&dir, opts());
        let elapsed = start.elapsed().as_micros() as u64;
        match opened {
            Ok(db) => {
                let last = format!("key{:012}", puts - 1);
                assert!(
                    db.get(last.as_bytes()).expect("get").is_some(),
                    "{label}: the last put did not survive"
                );
                // Every copy is opened once and never again: closed here, its
                // descriptors, lock and worker are given back, so a later
                // repetition is not refused for the files earlier ones held.
                db.close().expect("close");
                drop(db);
                fs::remove_dir_all(&dir).expect("remove the copy");
                micros.push(elapsed);
            }
            Err(e) => {
                println!("{label:<14} refused: {e}");
                return;
            }
        }
    }
    micros.sort_unstable();
    println!(
        "{label:<14} min {:8.2} ms  median {:8.2} ms  max {:8.2} ms",
        micros[0] as f64 / 1000.0,
        micros[micros.len() / 2] as f64 / 1000.0,
        micros[micros.len() - 1] as f64 / 1000.0,
    );
}

fn main() {
    let tables = env_u64("TABLES", 256);
    let leftover = env_u64("LOGS", 64);
    let tail_kib = env_u64("TAIL_KIB", 64);
    let reps = env_u64("REPS", 7).max(1);

    let scratch = tempfile::TempDir::new().expect("tempdir");
    let clean = scratch.path().join("clean");
    let puts = build(&clean, tables, 256);
    println!(
        "open_recovery: {tables} tables, {puts} puts, {} log(s) live",
        logs(&clean).len()
    );
    time_opens("clean", &clean, scratch.path(), reps, puts);

    let with_logs = scratch.path().join("leftover_logs");
    common::copy_db(&clean, &with_logs);
    let header = empty_log(scratch.path());
    // Below every live log, as a flush leaves the logs it could not remove.
    let live = logs(&clean)
        .iter()
        .filter_map(|p| {
            p.file_stem()?
                .to_str()?
                .strip_prefix("wal_")?
                .parse::<u64>()
                .ok()
        })
        .min()
        .expect("a live log");
    let leftover = leftover.min(live - 1);
    for id in 1..=leftover {
        fs::write(
            with_logs.join("wal").join(format!("wal_{id:06}.log")),
            &header,
        )
        .expect("write leftover log");
    }
    println!("leftover_logs: {leftover} logs below the live one");
    time_opens("leftover_logs", &with_logs, scratch.path(), reps, puts);

    let torn = scratch.path().join("torn_manifest");
    common::copy_db(&clean, &torn);
    let manifest = torn.join("MANIFEST");
    let mut bytes = fs::read(&manifest).expect("read");
    bytes.extend_from_slice(&garbage((tail_kib * 1024) as usize, 0x5EED));
    fs::write(&manifest, &bytes).expect("write");
    time_opens("torn_manifest", &torn, scratch.path(), reps, puts);
}
